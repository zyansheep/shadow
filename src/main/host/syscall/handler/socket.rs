use crate::cshadow as c;
use crate::host::descriptor::socket::inet::tcp::LegacyTcpSocket;
use crate::host::descriptor::socket::inet::InetSocket;
use crate::host::descriptor::socket::unix::{UnixSocket, UnixSocketType};
use crate::host::descriptor::socket::Socket;
use crate::host::descriptor::{
    CompatFile, Descriptor, DescriptorFlags, File, FileState, FileStatus, OpenFile,
};
use crate::host::syscall::handler::{
    read_sockaddr, write_sockaddr, SyscallContext, SyscallHandler,
};
use crate::host::syscall::type_formatting::{SyscallBufferArg, SyscallSockAddrArg};
use crate::host::syscall::Trigger;
use crate::host::syscall_condition::SysCallCondition;
use crate::host::syscall_types::{Blocked, PluginPtr, TypedPluginPtr};
use crate::host::syscall_types::{SyscallError, SyscallResult};
use crate::utility::callback_queue::CallbackQueue;
use crate::utility::sockaddr::SockaddrStorage;

use log::*;
use nix::errno::Errno;
use nix::sys::socket::{MsgFlags, Shutdown, SockFlag};

use syscall_logger::log_syscall;

impl SyscallHandler {
    #[log_syscall(/* rv */ libc::c_int, /* domain */ nix::sys::socket::AddressFamily,
                  /* type */ libc::c_int, /* protocol */ libc::c_int)]
    pub fn socket(
        ctx: &mut SyscallContext,
        domain: libc::c_int,
        socket_type: libc::c_int,
        protocol: libc::c_int,
    ) -> SyscallResult {
        // remove any flags from the socket type
        let flags = socket_type & (libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC);
        let socket_type = socket_type & !flags;

        // if it's not a unix socket or tcp socket, use the C syscall handler instead
        if domain != libc::AF_UNIX && (domain != libc::AF_INET || socket_type != libc::SOCK_STREAM)
        {
            return Self::legacy_syscall(c::syscallhandler_socket, ctx);
        }

        let mut file_flags = FileStatus::empty();
        let mut descriptor_flags = DescriptorFlags::empty();

        if flags & libc::SOCK_NONBLOCK != 0 {
            file_flags.insert(FileStatus::NONBLOCK);
        }

        if flags & libc::SOCK_CLOEXEC != 0 {
            descriptor_flags.insert(DescriptorFlags::CLOEXEC);
        }

        let socket = match domain {
            libc::AF_UNIX => {
                let socket_type = match UnixSocketType::try_from(socket_type) {
                    Ok(x) => x,
                    Err(e) => {
                        warn!("{}", e);
                        return Err(Errno::EPROTONOSUPPORT.into());
                    }
                };

                // unix sockets don't support any protocols
                if protocol != 0 {
                    warn!(
                        "Unsupported socket protocol {}, we only support default protocol 0",
                        protocol
                    );
                    return Err(Errno::EPROTONOSUPPORT.into());
                }

                Socket::Unix(UnixSocket::new(
                    file_flags,
                    socket_type,
                    &ctx.objs.host.abstract_unix_namespace(),
                ))
            }
            libc::AF_INET => match socket_type {
                libc::SOCK_STREAM => {
                    if protocol != 0 && protocol != libc::IPPROTO_TCP {
                        warn!("Unsupported inet stream socket protocol {protocol}");
                        return Err(Errno::EPROTONOSUPPORT.into());
                    }
                    Socket::Inet(InetSocket::LegacyTcp(LegacyTcpSocket::new(
                        file_flags,
                        ctx.objs.host,
                    )))
                }
                _ => panic!("Should have called the C syscall handler"),
            },
            _ => return Err(Errno::EAFNOSUPPORT.into()),
        };

        let mut desc = Descriptor::new(CompatFile::New(OpenFile::new(File::Socket(socket))));
        desc.set_flags(descriptor_flags);

        let fd = ctx
            .objs
            .process
            .descriptor_table_borrow_mut()
            .register_descriptor(desc)
            .or(Err(Errno::ENFILE))?;

        log::trace!("Created socket fd {}", fd);

        Ok(fd.val().into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int,
                  /* addr */ SyscallSockAddrArg</* addrlen */ 2>, /* addrlen */ libc::socklen_t)]
    pub fn bind(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len: libc::socklen_t,
    ) -> SyscallResult {
        let file = {
            // get the descriptor, or return early if it doesn't exist
            let desc_table = ctx.objs.process.descriptor_table_borrow();
            let desc = Self::get_descriptor(&desc_table, fd)?;

            let file = match desc.file() {
                CompatFile::New(file) => file,
                // if it's a legacy file, use the C syscall handler instead
                CompatFile::Legacy(_) => {
                    drop(desc_table);
                    return Self::legacy_syscall(c::syscallhandler_bind, ctx);
                }
            };

            file.inner_file().clone()
        };

        let File::Socket(ref socket) = file else {
            return Err(Errno::ENOTSOCK.into());
        };

        let addr = read_sockaddr(&ctx.objs.process.memory_borrow(), addr_ptr, addr_len)?;

        log::trace!("Attempting to bind fd {} to {:?}", fd, addr);

        let mut rng = ctx.objs.host.random_mut();
        let net_ns = ctx.objs.host.network_namespace_borrow();
        Socket::bind(socket, addr.as_ref(), &net_ns, &mut *rng)
    }

    #[log_syscall(/* rv */ libc::ssize_t, /* sockfd */ libc::c_int,
                  /* buf */ SyscallBufferArg</* len */ 2>, /* len */ libc::size_t,
                  /* flags */ nix::sys::socket::MsgFlags,
                  /* dest_addr */ SyscallSockAddrArg</* addrlen */ 5>,
                  /* addrlen */ libc::socklen_t)]
    pub fn sendto(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        buf_ptr: PluginPtr,
        buf_len: libc::size_t,
        flags: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len: libc::socklen_t,
    ) -> SyscallResult {
        // if we were previously blocked, get the active file from the last syscall handler
        // invocation since it may no longer exist in the descriptor table
        let file = ctx
            .objs
            .thread
            .syscall_condition()
            // if this was for a C descriptor, then there won't be an active file object
            .and_then(|x| x.active_file().cloned());

        let file = match file {
            // we were previously blocked, so re-use the file from the previous syscall invocation
            Some(x) => x,
            // get the file from the descriptor table, or return early if it doesn't exist
            None => {
                let desc_table = ctx.objs.process.descriptor_table_borrow();
                match Self::get_descriptor(&desc_table, fd)?.file() {
                    CompatFile::New(file) => file.clone(),
                    // if it's a legacy file, use the C syscall handler instead
                    CompatFile::Legacy(_) => {
                        drop(desc_table);
                        return Self::legacy_syscall(c::syscallhandler_sendto, ctx);
                    }
                }
            }
        };

        if let File::Socket(Socket::Inet(InetSocket::LegacyTcp(_))) = file.inner_file() {
            return Self::legacy_syscall(c::syscallhandler_sendto, ctx);
        }

        Self::sendto_helper(ctx, file, buf_ptr, buf_len, flags, addr_ptr, addr_len)
    }

    pub fn sendto_helper(
        ctx: &mut SyscallContext,
        open_file: OpenFile,
        buf_ptr: PluginPtr,
        buf_len: libc::size_t,
        flags: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len: libc::socklen_t,
    ) -> SyscallResult {
        let File::Socket(ref socket) = open_file.inner_file() else {
            return Err(Errno::ENOTSOCK.into());
        };

        // get the send flags
        let flags = match MsgFlags::from_bits(flags) {
            Some(x) => x,
            None => {
                // linux doesn't return an error if there are unexpected flags
                warn!("Invalid sendto flags: {}", flags);
                MsgFlags::from_bits_truncate(flags)
            }
        };

        // MSG_NOSIGNAL is currently a no-op, since we haven't implemented the behavior
        // it's meant to disable.
        // TODO: Once we've implemented generating a SIGPIPE when the peer on a
        // stream-oriented socket has closed the connection, MSG_NOSIGNAL should
        // disable it.
        let supported_flags = MsgFlags::MSG_DONTWAIT | MsgFlags::MSG_NOSIGNAL;
        if flags.intersects(!supported_flags) {
            warn!("Unsupported sendto flags: {:?}", flags);
            return Err(Errno::EOPNOTSUPP.into());
        }

        let addr = read_sockaddr(&ctx.objs.process.memory_borrow(), addr_ptr, addr_len)?;

        debug!("Attempting to send {} bytes to {:?}", buf_len, addr);

        let file_status = socket.borrow().get_status();

        // call the socket's sendto(), and run any resulting events
        let result = CallbackQueue::queue_and_run(|cb_queue| {
            socket.borrow_mut().sendto(
                ctx.objs
                    .process
                    .memory_borrow()
                    .reader(TypedPluginPtr::new::<u8>(buf_ptr, buf_len)),
                addr,
                cb_queue,
            )
        });

        // if the syscall would block, it's a blocking descriptor, and the `MSG_DONTWAIT` flag is not set
        if result == Err(Errno::EWOULDBLOCK.into())
            && !file_status.contains(FileStatus::NONBLOCK)
            && !flags.contains(MsgFlags::MSG_DONTWAIT)
        {
            let trigger = Trigger::from_file(open_file.inner_file().clone(), FileState::WRITABLE);
            let mut cond = SysCallCondition::new(trigger);
            let supports_sa_restart = socket.borrow().supports_sa_restart();
            cond.set_active_file(open_file);

            return Err(SyscallError::Blocked(Blocked {
                condition: cond,
                restartable: supports_sa_restart,
            }));
        };

        result
    }

    #[log_syscall(/* rv */ libc::ssize_t, /* sockfd */ libc::c_int, /* buf */ *const libc::c_void,
                  /* len */ libc::size_t, /* flags */ nix::sys::socket::MsgFlags,
                  /* src_addr */ *const libc::sockaddr, /* addrlen */ *const libc::socklen_t)]
    pub fn recvfrom(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        buf_ptr: PluginPtr,
        buf_len: libc::size_t,
        flags: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len_ptr: PluginPtr,
    ) -> SyscallResult {
        // if we were previously blocked, get the active file from the last syscall handler
        // invocation since it may no longer exist in the descriptor table
        let file = ctx
            .objs
            .thread
            .syscall_condition()
            // if this was for a C descriptor, then there won't be an active file object
            .and_then(|x| x.active_file().cloned());

        let file = match file {
            // we were previously blocked, so re-use the file from the previous syscall invocation
            Some(x) => x,
            // get the file from the descriptor table, or return early if it doesn't exist
            None => {
                let desc_table = ctx.objs.process.descriptor_table_borrow();
                match Self::get_descriptor(&desc_table, fd)?.file() {
                    CompatFile::New(file) => file.clone(),
                    // if it's a legacy file, use the C syscall handler instead
                    CompatFile::Legacy(_) => {
                        drop(desc_table);
                        return Self::legacy_syscall(c::syscallhandler_recvfrom, ctx);
                    }
                }
            }
        };

        if let File::Socket(Socket::Inet(InetSocket::LegacyTcp(_))) = file.inner_file() {
            return Self::legacy_syscall(c::syscallhandler_recvfrom, ctx);
        }

        Self::recvfrom_helper(ctx, file, buf_ptr, buf_len, flags, addr_ptr, addr_len_ptr)
    }

    pub fn recvfrom_helper(
        ctx: &mut SyscallContext,
        open_file: OpenFile,
        buf_ptr: PluginPtr,
        buf_len: libc::size_t,
        flags: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len_ptr: PluginPtr,
    ) -> SyscallResult {
        let File::Socket(ref socket) = open_file.inner_file() else {
            return Err(Errno::ENOTSOCK.into());
        };

        // get the recv flags
        let flags = match MsgFlags::from_bits(flags) {
            Some(x) => x,
            None => {
                // linux doesn't return an error if there are unexpected flags
                warn!("Invalid recvfrom flags: {}", flags);
                MsgFlags::from_bits_truncate(flags)
            }
        };

        let supported_flags = MsgFlags::MSG_DONTWAIT;
        if flags.intersects(!supported_flags) {
            warn!("Unsupported recvfrom flags: {:?}", flags);
            return Err(Errno::EOPNOTSUPP.into());
        }

        debug!("Attempting to recv {} bytes", buf_len);

        let file_status = socket.borrow().get_status();

        // call the socket's recvfrom(), and run any resulting events
        let result = CallbackQueue::queue_and_run(|cb_queue| {
            socket.borrow_mut().recvfrom(
                ctx.objs
                    .process
                    .memory_borrow_mut()
                    .writer(TypedPluginPtr::new::<u8>(buf_ptr, buf_len)),
                cb_queue,
            )
        });

        // if the syscall would block, it's a blocking descriptor, and the `MSG_DONTWAIT` flag is not set
        if matches!(result, Err(ref err) if err == &Errno::EWOULDBLOCK.into())
            && !file_status.contains(FileStatus::NONBLOCK)
            && !flags.contains(MsgFlags::MSG_DONTWAIT)
        {
            let trigger = Trigger::from_file(open_file.inner_file().clone(), FileState::READABLE);
            let mut cond = SysCallCondition::new(trigger);
            let supports_sa_restart = socket.borrow().supports_sa_restart();
            cond.set_active_file(open_file);

            return Err(SyscallError::Blocked(Blocked {
                condition: cond,
                restartable: supports_sa_restart,
            }));
        };

        let (result, from_addr) = result?;

        if !addr_ptr.is_null() {
            write_sockaddr(
                &mut ctx.objs.process.memory_borrow_mut(),
                from_addr.as_ref(),
                addr_ptr,
                TypedPluginPtr::new::<libc::socklen_t>(addr_len_ptr, 1),
            )?;
        }

        Ok(result)
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int, /* addr */ *const libc::sockaddr,
                  /* addrlen */ *const libc::socklen_t)]
    pub fn getsockname(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len_ptr: PluginPtr,
    ) -> SyscallResult {
        let addr_len_ptr = TypedPluginPtr::new::<libc::socklen_t>(addr_len_ptr, 1);

        let addr_to_write: Option<SockaddrStorage> = {
            // get the descriptor, or return early if it doesn't exist
            let desc_table = ctx.objs.process.descriptor_table_borrow();
            let desc = Self::get_descriptor(&desc_table, fd)?;

            let file = match desc.file() {
                CompatFile::New(file) => file,
                // if it's a legacy file, use the C syscall handler instead
                CompatFile::Legacy(_) => {
                    drop(desc_table);
                    return Self::legacy_syscall(c::syscallhandler_getsockname, ctx);
                }
            };

            let File::Socket(socket) = file.inner_file() else {
                return Err(Errno::ENOTSOCK.into());
            };

            // linux will return an EFAULT before other errors
            if addr_ptr.is_null() || addr_len_ptr.is_null() {
                return Err(Errno::EFAULT.into());
            }

            let socket = socket.borrow();
            socket.getsockname()?
        };

        debug!("Returning socket address of {:?}", addr_to_write);
        write_sockaddr(
            &mut ctx.objs.process.memory_borrow_mut(),
            addr_to_write.as_ref(),
            addr_ptr,
            addr_len_ptr,
        )?;

        Ok(0.into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int, /* addr */ *const libc::sockaddr,
                  /* addrlen */ *const libc::socklen_t)]
    pub fn getpeername(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len_ptr: PluginPtr,
    ) -> SyscallResult {
        let addr_len_ptr = TypedPluginPtr::new::<libc::socklen_t>(addr_len_ptr, 1);

        let addr_to_write = {
            // get the descriptor, or return early if it doesn't exist
            let desc_table = ctx.objs.process.descriptor_table_borrow();
            let desc = Self::get_descriptor(&desc_table, fd)?;

            let file = match desc.file() {
                CompatFile::New(file) => file,
                // if it's a legacy file, use the C syscall handler instead
                CompatFile::Legacy(_) => {
                    drop(desc_table);
                    return Self::legacy_syscall(c::syscallhandler_getpeername, ctx);
                }
            };

            let File::Socket(socket) = file.inner_file() else {
                return Err(Errno::ENOTSOCK.into());
            };

            // linux will return an EFAULT before other errors like ENOTCONN
            if addr_ptr.is_null() || addr_len_ptr.is_null() {
                return Err(Errno::EFAULT.into());
            }

            let addr_to_write = socket.borrow().getpeername()?;
            addr_to_write
        };

        debug!("Returning peer address of {:?}", addr_to_write);
        write_sockaddr(
            &mut ctx.objs.process.memory_borrow_mut(),
            addr_to_write.as_ref(),
            addr_ptr,
            addr_len_ptr,
        )?;

        Ok(0.into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int, /* backlog */ libc::c_int)]
    pub fn listen(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        backlog: libc::c_int,
    ) -> SyscallResult {
        // get the descriptor, or return early if it doesn't exist
        let desc_table = ctx.objs.process.descriptor_table_borrow();
        let desc = Self::get_descriptor(&desc_table, fd)?;

        let file = match desc.file() {
            CompatFile::New(file) => file,
            // if it's a legacy file, use the C syscall handler instead
            CompatFile::Legacy(_) => {
                drop(desc_table);
                return Self::legacy_syscall(c::syscallhandler_listen, ctx);
            }
        };

        let File::Socket(socket) = file.inner_file() else {
            drop(desc_table);
            return Err(Errno::ENOTSOCK.into());
        };

        let mut rng = ctx.objs.host.random_mut();
        let net_ns = ctx.objs.host.network_namespace_borrow();

        crate::utility::legacy_callback_queue::with_global_cb_queue(|| {
            CallbackQueue::queue_and_run(|cb_queue| {
                Socket::listen(socket, backlog, &net_ns, &mut *rng, cb_queue)
            })
        })?;

        Ok(0.into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int, /* addr */ *const libc::sockaddr,
                  /* addrlen */ *const libc::socklen_t)]
    pub fn accept(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len_ptr: PluginPtr,
    ) -> SyscallResult {
        // if we were previously blocked, get the active file from the last syscall handler
        // invocation since it may no longer exist in the descriptor table
        let file = ctx
            .objs
            .thread
            .syscall_condition()
            // if this was for a C descriptor, then there won't be an active file object
            .and_then(|x| x.active_file().cloned());

        let file = match file {
            // we were previously blocked, so re-use the file from the previous syscall invocation
            Some(x) => x,
            // get the file from the descriptor table, or return early if it doesn't exist
            None => {
                let desc_table = ctx.objs.process.descriptor_table_borrow();
                match Self::get_descriptor(&desc_table, fd)?.file() {
                    CompatFile::New(file) => file.clone(),
                    // if it's a legacy file, use the C syscall handler instead
                    CompatFile::Legacy(_) => {
                        drop(desc_table);
                        return Self::legacy_syscall(c::syscallhandler_accept, ctx);
                    }
                }
            }
        };

        Self::accept_helper(ctx, file, addr_ptr, addr_len_ptr, 0)
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int, /* addr */ *const libc::sockaddr,
                  /* addrlen */ *const libc::socklen_t, /* flags */ libc::c_int)]
    pub fn accept4(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len_ptr: PluginPtr,
        flags: libc::c_int,
    ) -> SyscallResult {
        // if we were previously blocked, get the active file from the last syscall handler
        // invocation since it may no longer exist in the descriptor table
        let file = ctx
            .objs
            .thread
            .syscall_condition()
            // if this was for a C descriptor, then there won't be an active file object
            .and_then(|x| x.active_file().cloned());

        let file = match file {
            // we were previously blocked, so re-use the file from the previous syscall invocation
            Some(x) => x,
            // get the file from the descriptor table, or return early if it doesn't exist
            None => {
                let desc_table = ctx.objs.process.descriptor_table_borrow();
                match Self::get_descriptor(&desc_table, fd)?.file() {
                    CompatFile::New(file) => file.clone(),
                    // if it's a legacy file, use the C syscall handler instead
                    CompatFile::Legacy(_) => {
                        drop(desc_table);
                        return Self::legacy_syscall(c::syscallhandler_accept4, ctx);
                    }
                }
            }
        };

        Self::accept_helper(ctx, file, addr_ptr, addr_len_ptr, flags)
    }

    fn accept_helper(
        ctx: &mut SyscallContext,
        open_file: OpenFile,
        addr_ptr: PluginPtr,
        addr_len_ptr: PluginPtr,
        flags: libc::c_int,
    ) -> SyscallResult {
        let File::Socket(ref socket) = open_file.inner_file() else {
            return Err(Errno::ENOTSOCK.into());
        };

        // get the accept flags
        let flags = match SockFlag::from_bits(flags) {
            Some(x) => x,
            None => {
                // linux doesn't return an error if there are unexpected flags
                warn!("Invalid recvfrom flags: {}", flags);
                SockFlag::from_bits_truncate(flags)
            }
        };

        let result = crate::utility::legacy_callback_queue::with_global_cb_queue(|| {
            CallbackQueue::queue_and_run(|cb_queue| socket.borrow_mut().accept(cb_queue))
        });

        let file_status = socket.borrow().get_status();

        // if the syscall would block and it's a blocking descriptor
        if result.as_ref().err() == Some(&Errno::EWOULDBLOCK.into())
            && !file_status.contains(FileStatus::NONBLOCK)
        {
            let trigger = Trigger::from_file(open_file.inner_file().clone(), FileState::READABLE);
            let mut cond = SysCallCondition::new(trigger);
            let supports_sa_restart = socket.borrow().supports_sa_restart();
            cond.set_active_file(open_file);

            return Err(SyscallError::Blocked(Blocked {
                condition: cond,
                restartable: supports_sa_restart,
            }));
        }

        let new_socket = result?;

        let from_addr = {
            let File::Socket(new_socket) = new_socket.inner_file() else {
                panic!("Accepted file should be a socket");
            };
            new_socket.borrow().getpeername().unwrap()
        };

        if !addr_ptr.is_null() {
            write_sockaddr(
                &mut ctx.objs.process.memory_borrow_mut(),
                from_addr.as_ref(),
                addr_ptr,
                TypedPluginPtr::new::<libc::socklen_t>(addr_len_ptr, 1),
            )?;
        }

        if flags.contains(SockFlag::SOCK_NONBLOCK) {
            new_socket
                .inner_file()
                .borrow_mut()
                .set_status(FileStatus::NONBLOCK);
        }

        let mut new_desc = Descriptor::new(CompatFile::New(new_socket));

        if flags.contains(SockFlag::SOCK_CLOEXEC) {
            new_desc.set_flags(DescriptorFlags::CLOEXEC);
        }

        let new_fd = ctx
            .objs
            .process
            .descriptor_table_borrow_mut()
            .register_descriptor(new_desc)
            .or(Err(Errno::ENFILE))?;

        Ok(new_fd.val().into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int,
                  /* addr */ SyscallSockAddrArg</* addrlen */ 2>, /* addrlen */ libc::socklen_t)]
    pub fn connect(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        addr_ptr: PluginPtr,
        addr_len: libc::socklen_t,
    ) -> SyscallResult {
        // if we were previously blocked, get the active file from the last syscall handler
        // invocation since it may no longer exist in the descriptor table
        let file = ctx
            .objs
            .thread
            .syscall_condition()
            // if this was for a C descriptor, then there won't be an active file object
            .and_then(|x| x.active_file().cloned());

        let file = match file {
            // we were previously blocked, so re-use the file from the previous syscall invocation
            Some(x) => x,
            // get the file from the descriptor table, or return early if it doesn't exist
            None => {
                let desc_table = ctx.objs.process.descriptor_table_borrow();
                match Self::get_descriptor(&desc_table, fd)?.file() {
                    CompatFile::New(file) => file.clone(),
                    // if it's a legacy file, use the C syscall handler instead
                    CompatFile::Legacy(_) => {
                        drop(desc_table);
                        return Self::legacy_syscall(c::syscallhandler_connect, ctx);
                    }
                }
            }
        };

        let File::Socket(socket) = file.inner_file() else {
            return Err(Errno::ENOTSOCK.into());
        };

        let addr = read_sockaddr(&ctx.objs.process.memory_borrow(), addr_ptr, addr_len)?
            .ok_or(Errno::EFAULT)?;

        let mut rng = ctx.objs.host.random_mut();
        let net_ns = ctx.objs.host.network_namespace_borrow();

        let mut rv = crate::utility::legacy_callback_queue::with_global_cb_queue(|| {
            CallbackQueue::queue_and_run(|cb_queue| {
                Socket::connect(socket, &addr, &net_ns, &mut *rng, cb_queue)
            })
        });

        // if we will block
        if let Err(SyscallError::Blocked(ref mut blocked)) = rv {
            // make sure the file does not close before the blocking syscall completes
            blocked.condition.set_active_file(file);
        }

        rv?;

        Ok(0.into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int, /* how */ libc::c_int)]
    pub fn shutdown(ctx: &mut SyscallContext, fd: libc::c_int, how: libc::c_int) -> SyscallResult {
        // get the descriptor, or return early if it doesn't exist
        let desc_table = ctx.objs.process.descriptor_table_borrow();
        let desc = Self::get_descriptor(&desc_table, fd)?;

        let file = match desc.file() {
            CompatFile::New(file) => file,
            // if it's a legacy file, use the C syscall handler instead
            CompatFile::Legacy(_) => {
                drop(desc_table);
                return Self::legacy_syscall(c::syscallhandler_shutdown, ctx);
            }
        };

        let how = match how {
            libc::SHUT_RD => Shutdown::Read,
            libc::SHUT_WR => Shutdown::Write,
            libc::SHUT_RDWR => Shutdown::Both,
            _ => return Err(Errno::EINVAL.into()),
        };

        let File::Socket(socket) = file.inner_file() else {
            drop(desc_table);
            return Err(Errno::ENOTSOCK.into());
        };

        crate::utility::legacy_callback_queue::with_global_cb_queue(|| {
            CallbackQueue::queue_and_run(|cb_queue| socket.borrow_mut().shutdown(how, cb_queue))
        })?;

        Ok(0.into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* domain */ nix::sys::socket::AddressFamily,
                  /* type */ libc::c_int, /* protocol */ libc::c_int, /* sv */ [libc::c_int; 2])]
    pub fn socketpair(
        ctx: &mut SyscallContext,
        domain: libc::c_int,
        socket_type: libc::c_int,
        protocol: libc::c_int,
        fd_ptr: PluginPtr,
    ) -> SyscallResult {
        // remove any flags from the socket type
        let flags = socket_type & (libc::SOCK_NONBLOCK | libc::SOCK_CLOEXEC);
        let socket_type = socket_type & !flags;

        // only AF_UNIX (AF_LOCAL) is supported on Linux (and technically AF_TIPC)
        if domain != libc::AF_UNIX {
            warn!("Domain {} is not supported for socketpair()", domain);
            return Err(Errno::EOPNOTSUPP.into());
        }

        let socket_type = match UnixSocketType::try_from(socket_type) {
            Ok(x) => x,
            Err(e) => {
                warn!("{}", e);
                return Err(Errno::EPROTONOSUPPORT.into());
            }
        };

        // unix sockets don't support any protocols
        if protocol != 0 {
            warn!(
                "Unsupported socket protocol {}, we only support default protocol 0",
                protocol
            );
            return Err(Errno::EPROTONOSUPPORT.into());
        }

        let mut file_flags = FileStatus::empty();
        let mut descriptor_flags = DescriptorFlags::empty();

        if flags & libc::SOCK_NONBLOCK != 0 {
            file_flags.insert(FileStatus::NONBLOCK);
        }

        if flags & libc::SOCK_CLOEXEC != 0 {
            descriptor_flags.insert(DescriptorFlags::CLOEXEC);
        }

        let (socket_1, socket_2) = CallbackQueue::queue_and_run(|cb_queue| {
            UnixSocket::pair(
                file_flags,
                socket_type,
                &ctx.objs.host.abstract_unix_namespace(),
                cb_queue,
            )
        });

        // file descriptors for the sockets
        let mut desc_1 = Descriptor::new(CompatFile::New(OpenFile::new(File::Socket(
            Socket::Unix(socket_1),
        ))));
        let mut desc_2 = Descriptor::new(CompatFile::New(OpenFile::new(File::Socket(
            Socket::Unix(socket_2),
        ))));

        // set the file descriptor flags
        desc_1.set_flags(descriptor_flags);
        desc_2.set_flags(descriptor_flags);

        // register the file descriptors
        let mut dt = ctx.objs.process.descriptor_table_borrow_mut();
        // unwrap here since the error handling would be messy (need to deregister) and we shouldn't
        // ever need to worry about this in practice
        let fd_1 = dt.register_descriptor(desc_1).unwrap();
        let fd_2 = dt.register_descriptor(desc_2).unwrap();

        // try to write them to the caller
        let fds = [i32::from(fd_1), i32::from(fd_2)];
        let write_res = ctx
            .objs
            .process
            .memory_borrow_mut()
            .copy_to_ptr(TypedPluginPtr::new::<libc::c_int>(fd_ptr, 2), &fds);

        // clean up in case of error
        match write_res {
            Ok(_) => Ok(0.into()),
            Err(e) => {
                CallbackQueue::queue_and_run(|cb_queue| {
                    // ignore any errors when closing
                    dt.deregister_descriptor(fd_1)
                        .unwrap()
                        .close(ctx.objs.host, cb_queue);
                    dt.deregister_descriptor(fd_2)
                        .unwrap()
                        .close(ctx.objs.host, cb_queue);
                });
                Err(e.into())
            }
        }
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int, /* level */ libc::c_int,
                  /* optname */ libc::c_int, /* optval */ *const libc::c_void,
                  /* optlen */ *const libc::socklen_t)]
    pub fn getsockopt(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        level: libc::c_int,
        optname: libc::c_int,
        optval_ptr: PluginPtr,
        optlen_ptr: PluginPtr,
    ) -> SyscallResult {
        // get the descriptor, or return early if it doesn't exist
        let desc_table = ctx.objs.process.descriptor_table_borrow();
        let desc = Self::get_descriptor(&desc_table, fd)?;

        let file = match desc.file() {
            CompatFile::New(file) => file,
            // if it's a legacy file, use the C syscall handler instead
            CompatFile::Legacy(_) => {
                drop(desc_table);
                return Self::legacy_syscall(c::syscallhandler_getsockopt, ctx);
            }
        };

        let File::Socket(socket) = file.inner_file() else {
            return Err(Errno::ENOTSOCK.into());
        };

        let mut mem = ctx.objs.process.memory_borrow_mut();

        // get the provided optlen
        let optlen_ptr = TypedPluginPtr::new::<libc::socklen_t>(optlen_ptr, 1);
        let optlen = mem.read_vals::<_, 1>(optlen_ptr)?[0];

        let mut optlen_new = socket
            .borrow()
            .getsockopt(level, optname, optval_ptr, optlen, &mut mem)?;

        if optlen_new > optlen {
            // this is probably a bug in the socket's getsockopt implementation
            log::warn!(
                "Attempting to return an optlen {} that's greater than the provided optlen {}",
                optlen_new,
                optlen
            );
            optlen_new = optlen;
        }

        // write the new optlen back to the plugin
        mem.copy_to_ptr(optlen_ptr, &[optlen_new])?;

        Ok(0.into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* sockfd */ libc::c_int, /* level */ libc::c_int,
                  /* optname */ libc::c_int, /* optval */ *const libc::c_void,
                  /* optlen */ libc::socklen_t)]
    pub fn setsockopt(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        level: libc::c_int,
        optname: libc::c_int,
        optval_ptr: PluginPtr,
        optlen: libc::socklen_t,
    ) -> SyscallResult {
        // get the descriptor, or return early if it doesn't exist
        let desc_table = ctx.objs.process.descriptor_table_borrow();
        let desc = Self::get_descriptor(&desc_table, fd)?;

        let file = match desc.file() {
            CompatFile::New(file) => file,
            // if it's a legacy file, use the C syscall handler instead
            CompatFile::Legacy(_) => {
                drop(desc_table);
                return Self::legacy_syscall(c::syscallhandler_setsockopt, ctx);
            }
        };

        let File::Socket(socket) = file.inner_file() else {
            drop(desc_table);
            return Err(Errno::ENOTSOCK.into());
        };

        let mem = ctx.objs.process.memory_borrow();

        socket
            .borrow_mut()
            .setsockopt(level, optname, optval_ptr, optlen, &mem)?;

        Ok(0.into())
    }
}
