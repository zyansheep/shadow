use crate::cshadow as c;
use crate::host::descriptor::pipe;
use crate::host::descriptor::shared_buf::SharedBuf;
use crate::host::descriptor::socket::inet::InetSocket;
use crate::host::descriptor::socket::Socket;
use crate::host::descriptor::{
    CompatFile, Descriptor, DescriptorFlags, File, FileMode, FileState, FileStatus, OpenFile,
};
use crate::host::syscall::handler::{SyscallContext, SyscallHandler};
use crate::host::syscall::type_formatting::SyscallBufferArg;
use crate::host::syscall::Trigger;
use crate::host::syscall_condition::SysCallCondition;
use crate::host::syscall_types::{Blocked, PluginPtr, TypedPluginPtr};
use crate::host::syscall_types::{SyscallError, SyscallResult};
use crate::utility::callback_queue::CallbackQueue;

use std::sync::Arc;

use atomic_refcell::AtomicRefCell;
use log::*;
use nix::errno::Errno;

use syscall_logger::log_syscall;

impl SyscallHandler {
    #[log_syscall(/* rv */ libc::c_int, /* fd */ libc::c_int)]
    pub fn close(ctx: &mut SyscallContext, fd: libc::c_int) -> SyscallResult {
        trace!("Trying to close fd {}", fd);

        let fd = fd.try_into().or(Err(nix::errno::Errno::EBADF))?;

        // according to "man 2 close", in Linux any errors that may occur will happen after the fd is
        // released, so we should always deregister the descriptor even if there's an error while
        // closing
        let desc = ctx
            .objs
            .process
            .descriptor_table_borrow_mut()
            .deregister_descriptor(fd)
            .ok_or(nix::errno::Errno::EBADF)?;

        // if there are still valid descriptors to the open file, close() will do nothing
        // and return None
        crate::utility::legacy_callback_queue::with_global_cb_queue(|| {
            CallbackQueue::queue_and_run(|cb_queue| desc.close(ctx.objs.host, cb_queue))
                .unwrap_or(Ok(()))
                .map(|()| 0.into())
        })
    }

    #[log_syscall(/* rv */ libc::c_int, /* oldfd */ libc::c_int)]
    pub fn dup(ctx: &mut SyscallContext, fd: libc::c_int) -> SyscallResult {
        // get the descriptor, or return early if it doesn't exist
        let mut desc_table = ctx.objs.process.descriptor_table_borrow_mut();
        let desc = Self::get_descriptor(&desc_table, fd)?;

        // duplicate the descriptor
        let new_desc = desc.dup(DescriptorFlags::empty());
        let new_fd = desc_table
            .register_descriptor(new_desc)
            .or(Err(Errno::ENFILE))?;

        // return the new fd
        Ok(libc::c_int::try_from(new_fd).unwrap().into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* oldfd */ libc::c_int, /* newfd */ libc::c_int)]
    pub fn dup2(
        ctx: &mut SyscallContext,
        old_fd: libc::c_int,
        new_fd: libc::c_int,
    ) -> SyscallResult {
        // get the descriptor, or return early if it doesn't exist
        let mut desc_table = ctx.objs.process.descriptor_table_borrow_mut();
        let desc = Self::get_descriptor(&desc_table, old_fd)?;

        // from 'man 2 dup2': "If oldfd is a valid file descriptor, and newfd has the same
        // value as oldfd, then dup2() does nothing, and returns newfd"
        if old_fd == new_fd {
            return Ok(new_fd.into());
        }

        let new_fd = new_fd.try_into().or(Err(nix::errno::Errno::EBADF))?;

        // duplicate the descriptor
        let new_desc = desc.dup(DescriptorFlags::empty());
        let replaced_desc = desc_table.register_descriptor_with_fd(new_desc, new_fd);

        // close the replaced descriptor
        if let Some(replaced_desc) = replaced_desc {
            // from 'man 2 dup2': "If newfd was open, any errors that would have been reported at
            // close(2) time are lost"
            CallbackQueue::queue_and_run(|cb_queue| replaced_desc.close(ctx.objs.host, cb_queue));
        }

        // return the new fd
        Ok(libc::c_int::from(new_fd).into())
    }

    #[log_syscall(/* rv */ libc::c_int, /* oldfd */ libc::c_int, /* newfd */ libc::c_int,
                  /* flags */ nix::fcntl::OFlag)]
    pub fn dup3(
        ctx: &mut SyscallContext,
        old_fd: libc::c_int,
        new_fd: libc::c_int,
        flags: libc::c_int,
    ) -> SyscallResult {
        // get the descriptor, or return early if it doesn't exist
        let mut desc_table = ctx.objs.process.descriptor_table_borrow_mut();
        let desc = Self::get_descriptor(&desc_table, old_fd)?;

        // from 'man 2 dup3': "If oldfd equals newfd, then dup3() fails with the error EINVAL"
        if old_fd == new_fd {
            return Err(nix::errno::Errno::EINVAL.into());
        }

        let new_fd = new_fd.try_into().or(Err(nix::errno::Errno::EBADF))?;

        // dup3 only supports the O_CLOEXEC flag
        let flags = match flags {
            libc::O_CLOEXEC => DescriptorFlags::CLOEXEC,
            0 => DescriptorFlags::empty(),
            _ => return Err(nix::errno::Errno::EINVAL.into()),
        };

        // duplicate the descriptor
        let new_desc = desc.dup(flags);
        let replaced_desc = desc_table.register_descriptor_with_fd(new_desc, new_fd);

        // close the replaced descriptor
        if let Some(replaced_desc) = replaced_desc {
            // from 'man 2 dup3': "If newfd was open, any errors that would have been reported at
            // close(2) time are lost"
            CallbackQueue::queue_and_run(|cb_queue| replaced_desc.close(ctx.objs.host, cb_queue));
        }

        // return the new fd
        Ok(libc::c_int::try_from(new_fd).unwrap().into())
    }

    #[log_syscall(/* rv */ libc::ssize_t, /* fd */ libc::c_int, /* buf */ *const libc::c_void,
                  /* count */ libc::size_t)]
    pub fn read(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        buf_ptr: PluginPtr,
        buf_size: libc::size_t,
    ) -> SyscallResult {
        let offset = 0;

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
                        return Self::legacy_syscall(c::syscallhandler_read, ctx);
                    }
                }
            }
        };

        if let File::Socket(Socket::Inet(InetSocket::LegacyTcp(_))) = file.inner_file() {
            return Self::legacy_syscall(c::syscallhandler_read, ctx);
        }

        Self::read_helper(ctx, fd, file, buf_ptr, buf_size, offset)
    }

    #[log_syscall(/* rv */ libc::ssize_t, /* fd */ libc::c_int, /* buf */ *const libc::c_void,
                  /* count */ libc::size_t, /* offset */ libc::off_t)]
    pub fn pread64(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        buf_ptr: PluginPtr,
        buf_size: libc::size_t,
        offset: libc::off_t,
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
                        return Self::legacy_syscall(c::syscallhandler_pread64, ctx);
                    }
                }
            }
        };

        if let File::Socket(Socket::Inet(InetSocket::LegacyTcp(_))) = file.inner_file() {
            return Self::legacy_syscall(c::syscallhandler_pread64, ctx);
        }

        Self::read_helper(ctx, fd, file, buf_ptr, buf_size, offset)
    }

    fn read_helper(
        ctx: &mut SyscallContext,
        _fd: libc::c_int,
        open_file: OpenFile,
        buf_ptr: PluginPtr,
        buf_size: libc::size_t,
        offset: libc::off_t,
    ) -> SyscallResult {
        let generic_file = open_file.inner_file();

        // if it's a socket, call recvfrom() instead
        if let File::Socket(..) = generic_file {
            if offset != 0 {
                // sockets don't support offsets
                return Err(Errno::ESPIPE.into());
            }
            return Self::recvfrom_helper(
                ctx,
                open_file,
                buf_ptr,
                buf_size,
                0,
                PluginPtr::null(),
                PluginPtr::null(),
            );
        }

        let file_status = generic_file.borrow().get_status();

        let result =
            // call the file's read(), and run any resulting events
            CallbackQueue::queue_and_run(|cb_queue| {
                generic_file.borrow_mut().read(
                    ctx.objs.process.memory_borrow_mut().writer(TypedPluginPtr::new::<u8>(buf_ptr, buf_size)),
                    offset,
                    cb_queue,
                )
            });

        // if the syscall would block and it's a blocking descriptor
        if result == Err(Errno::EWOULDBLOCK.into()) && !file_status.contains(FileStatus::NONBLOCK) {
            let trigger = Trigger::from_file(open_file.inner_file().clone(), FileState::READABLE);
            let mut cond = SysCallCondition::new(trigger);
            let supports_sa_restart = generic_file.borrow().supports_sa_restart();
            cond.set_active_file(open_file);

            return Err(SyscallError::Blocked(Blocked {
                condition: cond,
                restartable: supports_sa_restart,
            }));
        }

        result
    }

    #[log_syscall(/* rv */ libc::ssize_t, /* fd */ libc::c_int,
                  /* buf */ SyscallBufferArg</* count */ 2>, /* count */ libc::size_t)]
    pub fn write(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        buf_ptr: PluginPtr,
        buf_size: libc::size_t,
    ) -> SyscallResult {
        let offset = 0;

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
                        return Self::legacy_syscall(c::syscallhandler_write, ctx);
                    }
                }
            }
        };

        if let File::Socket(Socket::Inet(InetSocket::LegacyTcp(_))) = file.inner_file() {
            return Self::legacy_syscall(c::syscallhandler_write, ctx);
        }

        Self::write_helper(ctx, fd, file, buf_ptr, buf_size, offset)
    }

    #[log_syscall(/* rv */ libc::ssize_t, /* fd */ libc::c_int,
                  /* buf */ SyscallBufferArg</* count */ 2>, /* count */ libc::size_t,
                  /* offset */ libc::off_t)]
    pub fn pwrite64(
        ctx: &mut SyscallContext,
        fd: libc::c_int,
        buf_ptr: PluginPtr,
        buf_size: libc::size_t,
        offset: libc::off_t,
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
                        return Self::legacy_syscall(c::syscallhandler_pwrite64, ctx);
                    }
                }
            }
        };

        if let File::Socket(Socket::Inet(InetSocket::LegacyTcp(_))) = file.inner_file() {
            return Self::legacy_syscall(c::syscallhandler_pwrite64, ctx);
        }

        Self::write_helper(ctx, fd, file, buf_ptr, buf_size, offset)
    }

    fn write_helper(
        ctx: &mut SyscallContext,
        _fd: libc::c_int,
        open_file: OpenFile,
        buf_ptr: PluginPtr,
        buf_size: libc::size_t,
        offset: libc::off_t,
    ) -> SyscallResult {
        let generic_file = open_file.inner_file();

        // if it's a socket, call recvfrom() instead
        if let File::Socket(..) = generic_file {
            if offset != 0 {
                // sockets don't support offsets
                return Err(Errno::ESPIPE.into());
            }
            return Self::sendto_helper(ctx, open_file, buf_ptr, buf_size, 0, PluginPtr::null(), 0);
        }

        let file_status = generic_file.borrow().get_status();

        let result =
            // call the file's write(), and run any resulting events
            CallbackQueue::queue_and_run(|cb_queue| {
                generic_file.borrow_mut().write(
                    ctx.objs.process.memory_borrow().reader(TypedPluginPtr::new::<u8>(buf_ptr, buf_size)),
                    offset,
                    cb_queue,
                )
            });

        // if the syscall would block and it's a blocking descriptor
        if result == Err(Errno::EWOULDBLOCK.into()) && !file_status.contains(FileStatus::NONBLOCK) {
            let trigger = Trigger::from_file(open_file.inner_file().clone(), FileState::WRITABLE);
            let mut cond = SysCallCondition::new(trigger);
            let supports_sa_restart = generic_file.borrow().supports_sa_restart();
            cond.set_active_file(open_file);

            return Err(SyscallError::Blocked(Blocked {
                condition: cond,
                restartable: supports_sa_restart,
            }));
        };

        result
    }

    #[log_syscall(/* rv */ libc::c_int, /* pipefd */ [libc::c_int; 2])]
    pub fn pipe(ctx: &mut SyscallContext, fd_ptr: PluginPtr) -> SyscallResult {
        Self::pipe_helper(ctx, fd_ptr, 0)
    }

    #[log_syscall(/* rv */ libc::c_int, /* pipefd */ [libc::c_int; 2],
                  /* flags */ nix::fcntl::OFlag)]
    pub fn pipe2(ctx: &mut SyscallContext, fd_ptr: PluginPtr, flags: libc::c_int) -> SyscallResult {
        Self::pipe_helper(ctx, fd_ptr, flags)
    }

    fn pipe_helper(ctx: &mut SyscallContext, fd_ptr: PluginPtr, flags: i32) -> SyscallResult {
        // make sure they didn't pass a NULL pointer
        if fd_ptr.is_null() {
            return Err(nix::errno::Errno::EFAULT.into());
        }

        let mut file_flags = FileStatus::empty();
        let mut descriptor_flags = DescriptorFlags::empty();

        // keep track of which flags we use
        let mut remaining_flags = flags;

        if flags & libc::O_NONBLOCK != 0 {
            file_flags.insert(FileStatus::NONBLOCK);
            remaining_flags &= !libc::O_NONBLOCK;
        }

        if flags & libc::O_DIRECT != 0 {
            file_flags.insert(FileStatus::DIRECT);
            remaining_flags &= !libc::O_DIRECT;
        }

        if flags & libc::O_CLOEXEC != 0 {
            descriptor_flags.insert(DescriptorFlags::CLOEXEC);
            remaining_flags &= !libc::O_CLOEXEC;
        }

        // the user requested flags that we don't support
        if remaining_flags != 0 {
            warn!("Ignoring pipe flags");
        }

        // reference-counted buffer for the pipe
        let buffer = SharedBuf::new(c::CONFIG_PIPE_BUFFER_SIZE.try_into().unwrap());
        let buffer = Arc::new(AtomicRefCell::new(buffer));

        // reference-counted file object for read end of the pipe
        let reader = pipe::Pipe::new(FileMode::READ, file_flags);
        let reader = Arc::new(AtomicRefCell::new(reader));

        // reference-counted file object for write end of the pipe
        let writer = pipe::Pipe::new(FileMode::WRITE, file_flags);
        let writer = Arc::new(AtomicRefCell::new(writer));

        // set the file objects to listen for events on the buffer
        CallbackQueue::queue_and_run(|cb_queue| {
            pipe::Pipe::connect_to_buffer(&reader, Arc::clone(&buffer), cb_queue);
            pipe::Pipe::connect_to_buffer(&writer, Arc::clone(&buffer), cb_queue);
        });

        // file descriptors for the read and write file objects
        let mut reader_desc = Descriptor::new(CompatFile::New(OpenFile::new(File::Pipe(reader))));
        let mut writer_desc = Descriptor::new(CompatFile::New(OpenFile::new(File::Pipe(writer))));

        // set the file descriptor flags
        reader_desc.set_flags(descriptor_flags);
        writer_desc.set_flags(descriptor_flags);

        // register the file descriptors
        let mut dt = ctx.objs.process.descriptor_table_borrow_mut();
        // unwrap here since the error handling would be messy (need to deregister) and we shouldn't
        // ever need to worry about this in practice
        let read_fd = dt.register_descriptor(reader_desc).unwrap();
        let write_fd = dt.register_descriptor(writer_desc).unwrap();

        // try to write them to the caller
        let fds = [
            i32::try_from(read_fd).unwrap(),
            i32::try_from(write_fd).unwrap(),
        ];
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
                    dt.deregister_descriptor(read_fd)
                        .unwrap()
                        .close(ctx.objs.host, cb_queue);
                    dt.deregister_descriptor(write_fd)
                        .unwrap()
                        .close(ctx.objs.host, cb_queue);
                });
                Err(e.into())
            }
        }
    }
}
