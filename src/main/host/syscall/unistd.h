/*
 * The Shadow Simulator
 * See LICENSE for licensing information
 */

#ifndef SRC_MAIN_HOST_SYSCALL_UNISTD_H_
#define SRC_MAIN_HOST_SYSCALL_UNISTD_H_

#include "main/host/syscall/protected.h"

SYSCALL_HANDLER(exit_group);
SYSCALL_HANDLER(getpid);
SYSCALL_HANDLER(getppid);
SYSCALL_HANDLER(pread64);
SYSCALL_HANDLER(pwrite64);
SYSCALL_HANDLER(read);
SYSCALL_HANDLER(set_tid_address);
SYSCALL_HANDLER(uname);
SYSCALL_HANDLER(write);

SysCallReturn _syscallhandler_readHelper(SysCallHandler* sys, int fd, PluginPtr bufPtr,
                                         size_t bufSize, off_t offset, bool doPread);

SysCallReturn _syscallhandler_writeHelper(SysCallHandler* sys, int fd, PluginPtr bufPtr,
                                          size_t bufSize, off_t offset, bool doPwrite);

#endif /* SRC_MAIN_HOST_SYSCALL_UNISTD_H_ */
