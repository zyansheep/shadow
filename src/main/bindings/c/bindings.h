/*
 * The Shadow Simulator
 * See LICENSE for licensing information
 */
// clang-format off


#ifndef main_bindings_h
#define main_bindings_h

/* Warning, this file is autogenerated by cbindgen. Don't modify this manually. */

#include <stdarg.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include "main/bindings/c/bindings-opaque.h"
#include "main/host/descriptor/descriptor_types.h"
#include "main/host/status_listener.h"
#include "main/host/syscall_handler.h"
#include "main/host/syscall_types.h"
#include "main/host/thread.h"

void rust_logging_init(void);

// The new compat descriptor takes ownership of the reference to the legacy descriptor and
// does not increment its ref count, but will decrement the ref count when this compat
// descriptor is freed/dropped.
struct CompatDescriptor *compatdescriptor_fromLegacy(LegacyDescriptor *legacy_descriptor);

// If the compat descriptor is a legacy descriptor, returns a pointer to the legacy
// descriptor object. Otherwise returns NULL. The legacy descriptor's ref count is not
// modified, so the pointer must not outlive the lifetime of the compat descriptor.
LegacyDescriptor *compatdescriptor_asLegacy(const struct CompatDescriptor *descriptor);

// When the compat descriptor is freed/dropped, it will decrement the legacy descriptor's
// ref count.
void compatdescriptor_free(struct CompatDescriptor *descriptor);

// This is a no-op for non-legacy descriptors.
void compatdescriptor_setHandle(struct CompatDescriptor *descriptor, int handle);

// If the compat descriptor is a new descriptor, returns a pointer to the reference-counted
// posix file object. Otherwise returns NULL. The posix file object's ref count is not
// modified, so the pointer must not outlive the lifetime of the compat descriptor.
const struct PosixFileArc *compatdescriptor_borrowPosixFile(struct CompatDescriptor *descriptor);

// If the compat descriptor is a new descriptor, returns a pointer to the reference-counted
// posix file object. Otherwise returns NULL. The posix file object's ref count is
// incremented, so the pointer must always later be passed to `posixfile_drop()`, otherwise
// the memory will leak.
const struct PosixFileArc *compatdescriptor_newRefPosixFile(struct CompatDescriptor *descriptor);

// Decrement the ref count of the posix file object. The pointer must not be used after
// calling this function.
void posixfile_drop(const struct PosixFileArc *file);

// Get the status of the posix file object.
Status posixfile_getStatus(const struct PosixFileArc *file);

// Add a status listener to the posix file object. This will increment the status
// listener's ref count, and will decrement the ref count when this status listener is
// removed or when the posix file is freed/dropped.
void posixfile_addListener(const struct PosixFileArc *file, StatusListener *listener);

// Remove a listener from the posix file object.
void posixfile_removeListener(const struct PosixFileArc *file, StatusListener *listener);

// # Safety
// * `thread` must point to a valid object.
struct MemoryManager *memorymanager_new(Thread *thread);

// # Safety
// * `mm` must point to a valid object.
void memorymanager_free(struct MemoryManager *mm);

// Get a readable pointer to the plugin's memory via mapping, or via the thread APIs.
// # Safety
// * `mm` and `thread` must point to valid objects.
const void *memorymanager_getReadablePtr(struct MemoryManager *memory_manager,
                                         Thread *thread,
                                         PluginPtr plugin_src,
                                         uintptr_t n);

// Get a writeable pointer to the plugin's memory via mapping, or via the thread APIs.
// # Safety
// * `mm` and `thread` must point to valid objects.
void *memorymanager_getWriteablePtr(struct MemoryManager *memory_manager,
                                    Thread *thread,
                                    PluginPtr plugin_src,
                                    uintptr_t n);

// Get a mutable pointer to the plugin's memory via mapping, or via the thread APIs.
// # Safety
// * `mm` and `thread` must point to valid objects.
void *memorymanager_getMutablePtr(struct MemoryManager *memory_manager,
                                  Thread *thread,
                                  PluginPtr plugin_src,
                                  uintptr_t n);

// Fully handles the `brk` syscall, keeping the "heap" mapped in our shared mem file.
SysCallReg memorymanager_handleBrk(struct MemoryManager *memory_manager,
                                   Thread *thread,
                                   PluginPtr plugin_src);

// Fully handles the `mmap` syscall
SysCallReg memorymanager_handleMmap(struct MemoryManager *memory_manager,
                                    Thread *thread,
                                    PluginPtr addr,
                                    uintptr_t len,
                                    int32_t prot,
                                    int32_t flags,
                                    int32_t fd,
                                    int64_t offset);

// Fully handles the `munmap` syscall
SysCallReg memorymanager_handleMunmap(struct MemoryManager *memory_manager,
                                      Thread *thread,
                                      PluginPtr addr,
                                      uintptr_t len);

SysCallReg memorymanager_handleMremap(struct MemoryManager *memory_manager,
                                      Thread *thread,
                                      PluginPtr old_addr,
                                      uintptr_t old_size,
                                      uintptr_t new_size,
                                      int32_t flags,
                                      PluginPtr new_addr);

SysCallReg memorymanager_handleMprotect(struct MemoryManager *memory_manager,
                                        Thread *thread,
                                        PluginPtr addr,
                                        uintptr_t size,
                                        int32_t prot);

SysCallReturn rustsyscallhandler_close(SysCallHandler *sys, const SysCallArgs *args);

SysCallReturn rustsyscallhandler_dup(SysCallHandler *sys, const SysCallArgs *args);

SysCallReturn rustsyscallhandler_read(SysCallHandler *sys, const SysCallArgs *args);

SysCallReturn rustsyscallhandler_pread64(SysCallHandler *sys, const SysCallArgs *args);

SysCallReturn rustsyscallhandler_write(SysCallHandler *sys, const SysCallArgs *args);

SysCallReturn rustsyscallhandler_pwrite64(SysCallHandler *sys, const SysCallArgs *args);

SysCallReturn rustsyscallhandler_pipe(SysCallHandler *sys, const SysCallArgs *args);

SysCallReturn rustsyscallhandler_pipe2(SysCallHandler *sys, const SysCallArgs *args);

struct ByteQueue *bytequeue_new(size_t chunk_size);

void bytequeue_free(struct ByteQueue *bq_ptr);

size_t bytequeue_len(struct ByteQueue *bq);

bool bytequeue_isEmpty(struct ByteQueue *bq);

void bytequeue_push(struct ByteQueue *bq, const unsigned char *src, size_t len);

size_t bytequeue_pop(struct ByteQueue *bq, unsigned char *dst, size_t len);

struct Counter *counter_new(void);

void counter_free(struct Counter *counter_ptr);

int64_t counter_add_value(struct Counter *counter, const char *id, int64_t value);

int64_t counter_sub_value(struct Counter *counter, const char *id, int64_t value);

void counter_add_counter(struct Counter *counter, struct Counter *other);

void counter_sub_counter(struct Counter *counter, struct Counter *other);

bool counter_equals_counter(const struct Counter *counter, const struct Counter *other);

// Creates a new string representation of the counter, e.g., for logging.
// The returned string must be free'd by passing it to counter_free_string.
char *counter_alloc_string(struct Counter *counter);

// Frees a string previously returned from counter_alloc_string.
void counter_free_string(struct Counter *counter, char *ptr);

#endif /* main_bindings_h */
