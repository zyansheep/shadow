// Defines system call wrappers: functions that are documented in man section 2. (See `man man`).
// This file defines ths symbols that will be included in the preload library,
// and we redirect to the syscall() function to actually handle them.

// Get the SYS_xxx definitions
#include <sys/syscall.h>

// External declarations, to minimize the headers we need to include
long syscall(long n, ...);

// Defines a thin wrapper function `func_name` that invokes the syscall `syscall_name`.
#define INTERPOSE_REMAP(func_name, syscall_name)                                                   \
    long func_name(long a, long b, long c, long d, long e, long f) {                               \
        return syscall(SYS_##syscall_name, a, b, c, d, e, f);                                      \
    }

// Defines a thin wrapper whose function name 'func_name' is the same as the syscall name
#define INTERPOSE(func_name) INTERPOSE_REMAP(func_name, func_name)

// Function definitions for the preloaded functions
// Note: send() and recv() are preloaded in preload_libraries.c
// clang-format off
INTERPOSE_REMAP(__fcntl, fcntl);
INTERPOSE_REMAP(creat64, creat);
INTERPOSE_REMAP(fallocate64, fallocate);
INTERPOSE_REMAP(fcntl64, fcntl);
INTERPOSE_REMAP(mmap64, mmap);
INTERPOSE_REMAP(open64, open);
INTERPOSE(accept);
INTERPOSE(accept4);
INTERPOSE(bind);
INTERPOSE(clock_gettime);
INTERPOSE(close);
INTERPOSE(connect);
INTERPOSE(creat);
INTERPOSE(dup);
INTERPOSE(epoll_create);
INTERPOSE(epoll_create1);
INTERPOSE(epoll_ctl);
INTERPOSE(epoll_wait);
INTERPOSE(eventfd);
INTERPOSE(eventfd2);
INTERPOSE(faccessat);
INTERPOSE(fadvise64);
INTERPOSE(fallocate);
INTERPOSE(fchdir);
INTERPOSE(fchmod);
INTERPOSE(fchmodat);
INTERPOSE(fchown);
INTERPOSE(fchownat);
INTERPOSE(fcntl);
INTERPOSE(fdatasync);
INTERPOSE(fgetxattr);
INTERPOSE(flistxattr);
INTERPOSE(flock);
INTERPOSE(fremovexattr);
INTERPOSE(fsetxattr);
INTERPOSE(fstat);
INTERPOSE(fstatfs);
INTERPOSE(fsync);
INTERPOSE(ftruncate);
INTERPOSE(futimesat);
INTERPOSE(getdents);
INTERPOSE(getdents64);
INTERPOSE(getpeername);
INTERPOSE(getpid);
INTERPOSE(getrandom);
INTERPOSE(getsockname);
INTERPOSE(getsockopt);
INTERPOSE(ioctl);
INTERPOSE(kill);
INTERPOSE(linkat);
INTERPOSE(listen);
INTERPOSE(lseek);
INTERPOSE(mkdirat);
INTERPOSE(mknodat);
INTERPOSE(mmap);
#ifdef SYS_mmap2
INTERPOSE(mmap2);
#endif
INTERPOSE(mremap);
INTERPOSE(munmap);
INTERPOSE(nanosleep);
INTERPOSE(newfstatat);
INTERPOSE(open);
INTERPOSE(openat);
INTERPOSE(pipe);
INTERPOSE(pipe2);
INTERPOSE(pread64);
INTERPOSE(preadv);
#ifdef SYS_preadv2
INTERPOSE(preadv2);
#endif
#ifdef SYS_prlimit
INTERPOSE(prlimit);
#endif
#ifdef SYS_prlimit64
INTERPOSE(prlimit64);
#endif
INTERPOSE(pwrite64);
INTERPOSE(pwritev);
#ifdef SYS_pwritev2
INTERPOSE(pwritev2);
#endif
INTERPOSE(read);
INTERPOSE(readahead);
INTERPOSE(readlinkat);
INTERPOSE(readv);
INTERPOSE(recvfrom);
INTERPOSE(renameat);
INTERPOSE(renameat2);
INTERPOSE(sendto);
INTERPOSE(setsockopt);
INTERPOSE(shutdown);
INTERPOSE(socket);
INTERPOSE(socketpair);
#ifdef SYS_statx
INTERPOSE(statx);
#endif
INTERPOSE(symlinkat);
INTERPOSE(sync_file_range);
INTERPOSE(syncfs);
INTERPOSE(tgkill);
INTERPOSE(tkill);
INTERPOSE(uname);
INTERPOSE(unlinkat);
INTERPOSE(utimensat);
INTERPOSE(write);
INTERPOSE(writev);
// clang-format on