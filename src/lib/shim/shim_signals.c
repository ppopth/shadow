#include "lib/shim/shim_signals.h"

#include "lib/linux-api/linux-api.h"
#include "lib/logger/logger.h"
#include "lib/shim/shim.h"
#include "lib/shim/shim_api.h"

#include <errno.h>
#include <string.h>
#include <ucontext.h>

static void _call_signal_handler(const struct linux_sigaction* action, int signo,
                                 linux_siginfo_t* siginfo, ucontext_t* ucontext) {
    shim_swapAllowNativeSyscalls(false);
    void (*action_fn)(int, linux_siginfo_t*, void*) = linux_sigaction_action(action);
    void (*handler_fn)(int) = linux_sigaction_handler(action);
    if (action_fn) {
        action_fn(signo, siginfo, ucontext);
    } else if (handler_fn) {
        handler_fn(signo);
    } else {
        panic("Unreachable");
    }
    shim_swapAllowNativeSyscalls(true);
}

static _Noreturn void _die_with_fatal_signal(int signo) {
    shim_swapAllowNativeSyscalls(true);
    // Deliver natively to terminate/drop core.
    if (signo == SIGKILL) {
        // No need to restore default action, and trying to do so would fail.
    } else if (sigaction(signo, &(struct sigaction){.sa_handler = SIG_DFL}, NULL) != 0) {
        panic("sigaction: %s", strerror(errno));
    }
    raise(signo);
    panic("Unreachable");
}

// Handle pending unblocked signals, and return whether *all* corresponding
// signal actions had the SA_RESTART flag set.
bool shim_process_signals(ShimShmemHostLock* host_lock, ucontext_t* ucontext) {
    int signo;
    linux_siginfo_t siginfo;
    bool restartable = true;
    while ((signo = shimshmem_takePendingUnblockedSignal(
                host_lock, shim_processSharedMem(), shim_threadSharedMem(), &siginfo)) != 0) {
        linux_sigset_t blocked_signals =
            shimshmem_getBlockedSignals(host_lock, shim_threadSharedMem());

        struct linux_sigaction action =
            shimshmem_getSignalAction(host_lock, shim_processSharedMem(), signo);

        if (linux_sigaction_is_ign(&action)) {
            continue;
        }

        if (linux_sigaction_is_dfl(&action)) {
            switch (linux_defaultAction(signo)) {
                case LINUX_DEFAULT_ACTION_IGN:
                    // Ignore
                    continue;
                case LINUX_DEFAULT_ACTION_CORE:
                case LINUX_DEFAULT_ACTION_TERM: {
                    // Deliver natively to terminate/drop core.
                    shimshmemhost_unlock(shim_hostSharedMem(), &host_lock);
                    _die_with_fatal_signal(signo);
                    panic("Unreachable");
                }
                case LINUX_DEFAULT_ACTION_STOP: panic("Stop via signal unimplemented.");
                case LINUX_DEFAULT_ACTION_CONT: panic("Continue via signal unimplemented.");
            };
            panic("Unreachable");
        }

        trace("Handling signo %d", signo);

        linux_sigset_t handler_mask = linux_sigorset(&blocked_signals, &action.lsa_mask);
        if (!(action.lsa_flags & SA_NODEFER)) {
            // Block another instance of the same signal.
            linux_sigaddset(&handler_mask, signo);
        }
        shimshmem_setBlockedSignals(host_lock, shim_threadSharedMem(), handler_mask);

        if (action.lsa_flags & SA_RESETHAND) {
            shimshmem_setSignalAction(host_lock, shim_processSharedMem(), signo,
                                      &(struct linux_sigaction){.lsa_handler = SIG_DFL});
        }
        if (!(action.lsa_flags & SA_RESTART)) {
            restartable = false;
        }

        const stack_t ss_original = shimshmem_getSigAltStack(host_lock, shim_threadSharedMem());
        if (action.lsa_flags & SA_ONSTACK && !(ss_original.ss_flags & SS_DISABLE)) {
            // Call handler on the configured signal stack.

            if (ss_original.ss_flags & SS_ONSTACK) {
                // Documentation is unclear what should happen, but switching to
                // the already-in-use stack would almost certainly go badly.
                panic("Alternate stack already in use.")
            }

            // Update the signal-stack configuration while the handler is being run.
            stack_t ss_during_handler;
            if (ss_original.ss_flags & LINUX_SS_AUTODISARM) {
                ss_during_handler = (stack_t){.ss_flags = SS_DISABLE};
            } else {
                ss_during_handler = ss_original;
                ss_during_handler.ss_flags |= SS_ONSTACK;
            }
            shimshmem_setSigAltStack(host_lock, shim_threadSharedMem(), ss_during_handler);

            // Set up a context that uses the configured signal stack.
            ucontext_t orig_ctx = {0}, handler_ctx = {0};
            getcontext(&handler_ctx);
            handler_ctx.uc_link = &orig_ctx;
            handler_ctx.uc_stack.ss_sp = ss_original.ss_sp;
            handler_ctx.uc_stack.ss_size = ss_original.ss_size;
            // If a context was provided by the caller, we pass that through
            // to the signal handler; it's the caller's responsibility to swap
            // back to that context.
            //
            // Otherwise we pass the pre-stack-switch context we're creating
            // here.  It'll be swapped-back-to when `swapcontext` returns.
            ucontext_t* ctx = ucontext ? ucontext : &orig_ctx;
            makecontext(&handler_ctx, (void (*)(void))_call_signal_handler, 4, &action, signo,
                        &siginfo, ctx);

            // Call the handler on the configured signal stack.
            shimshmemhost_unlock(shim_hostSharedMem(), &host_lock);
            if (swapcontext(&orig_ctx, &handler_ctx) != 0) {
                panic("swapcontext: %s", strerror(errno));
            }
            host_lock = shimshmemhost_lock(shim_hostSharedMem());

            // Restore the signal-stack configuration.
            shimshmem_setSigAltStack(host_lock, shim_threadSharedMem(), ss_original);
        } else {
            ucontext_t* ctx = ucontext;
            if (ctx == NULL) {
                // To handle this case we might be able to use `makecontext`
                // and `swapcontext` as in the sigaltstack case, but we'd need
                // a stack to use for the new context. We could try to partition
                // the current stack, but that's a bit tricky.
                //
                // So far we don't know of any real-world cases that get here
                // and actually dereference the context in the handler.
                debug("Passing NULL ucontext_t to handler for signal %d", signo);
            }

            // Call signal handler with host lock released, and native syscalls
            // disabled.
            shimshmemhost_unlock(shim_hostSharedMem(), &host_lock);
            _call_signal_handler(&action, signo, &siginfo, ctx);
            host_lock = shimshmemhost_lock(shim_hostSharedMem());
        }

        // Restore mask
        shimshmem_setBlockedSignals(host_lock, shim_threadSharedMem(), blocked_signals);
    }
    return restartable;
}

void shim_handle_hardware_error_signal(int signo, siginfo_t* info, void* void_ucontext) {
    bool oldNativeSyscallFlag = shim_swapAllowNativeSyscalls(true);
    if (oldNativeSyscallFlag) {
        // Error was raised from shim code.
        _die_with_fatal_signal(signo);
        panic("Unreachable");
    }
    // Otherwise the error was raised from managed code, and could potentially
    // be handled by a signal handler that it installed.

    ShimShmemHostLock* host_lock = shimshmemhost_lock(shim_hostSharedMem());

    linux_sigset_t pending_signals =
        shimshmem_getThreadPendingSignals(host_lock, shim_threadSharedMem());
    if (linux_sigismember(&pending_signals, signo)) {
        warning("Received signal %d when it was already pending", signo);
    } else {
        linux_sigaddset(&pending_signals, signo);
        shimshmem_setThreadPendingSignals(host_lock, shim_threadSharedMem(), pending_signals);
        // So far we've gotten away with assuming that the libc and kernel
        // siginfo_t's are the same.
        //
        // TODO: Use a raw SYS_sigaction syscall in the first place, so that we
        // can know for sure that we're getting the kernel definition in the
        // handler. e.g. wrap the syscall in linux-api, and have the handler
        // specifically get a `linux_siginfo_t`.
        shimshmem_setThreadSiginfo(
            host_lock, shim_threadSharedMem(), signo, (linux_siginfo_t*)info);
    }

    shim_process_signals(host_lock, void_ucontext);
    shimshmemhost_unlock(shim_hostSharedMem(), &host_lock);
    shim_swapAllowNativeSyscalls(oldNativeSyscallFlag);
}

void shim_install_hardware_error_handlers() {
    int error_signals[] = {
        SIGSEGV, SIGILL, SIGBUS, SIGFPE,
    };
    for (int i = 0; i < sizeof(error_signals) / sizeof(error_signals[0]); ++i) {
        if (sigaction(error_signals[i],
                      &(struct sigaction){
                          .sa_sigaction = shim_handle_hardware_error_signal,
                          // SA_NODEFER: Don't block the current signal in the handler.
                          // Generating one of these signals while it is blocked is
                          // undefined behavior; the handler itself detects recursion.
                          // SA_SIGINFO: Required because we're specifying
                          // sa_sigaction.
                          // SA_ONSTACK: Use the alternate signal handling stack,
                          // to avoid interfering with userspace thread stacks.
                          .sa_flags = SA_SIGINFO | SA_NODEFER | SA_ONSTACK,
                      },
                      NULL) < 0) {
            panic("sigaction: %s", strerror(errno));
        }
    }
}