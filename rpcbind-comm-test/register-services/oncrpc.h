/*
 * Minimal oncrpc header for stripped-down nfsd build.
 *
 * Declarations for rpcb_unset and rpcb_set used by register_services in
 * the nfsd main. This is intentionally minimal — it only declares the
 * functions as they are called from the tree. Numeric arguments are
 * unsigned int per BSD/Linux conventions.
 *
 * Assumptions:
 * - Return type is int (typical for these RPC registry helpers).
 * - program and version are unsigned int.
 * - networkid and addr are NUL-terminated C strings (const char *).
 */

#ifndef ONCRPC_H
#define ONCRPC_H

#ifdef __cplusplus
extern "C" {
#endif

#include <stddef.h>
#include <sys/socket.h>

/*
 * On macOS the system RPC implementation exports the names with a
 * "__newrpclib_" prefix (for example, `__newrpclib_rpcb_unset`). A
 * minimal local header may declare the traditional names (`rpcb_unset`,
 * `rpcb_set`) but the linker will fail because the exported symbols in
 * the system libraries actually have the prefixed names. To make code
 * compiled against this header link correctly on macOS, map the old
 * names to the real exported symbols.
 */
#if defined(__APPLE__)
#ifndef RPCB_NAME_MAPPED_TO_NEWRPCLIB
#define RPCB_NAME_MAPPED_TO_NEWRPCLIB 1
#define rpcb_unset _newrpclib_rpcb_unset
#define rpcb_set _newrpclib_rpcb_set
#endif
#endif

/* Unregister the (program, version) pair from the RPC binder.
 * The first argument is intentionally a generic pointer (NULL is passed
 * by callers to indicate all networks). Numeric arguments are unsigned
 * int as used in this tree.
 * Returns 0 on success, non-zero on failure (implementation-defined).
 */
int rpcb_unset(const char *netid, unsigned int program, unsigned int version);

/* Register the (program, version) pair with the RPC binder for the
 * provided network id and address. 'netid' is a NUL-terminated C string
 * identifying the transport (e.g. "udp", "tcp", "udp6", "tcp6").
 * 'addr' is a pointer to a struct sockaddr describing where the service
 * is bound (may be a sockaddr_in, sockaddr_in6 or sockaddr_un).
 * Returns 0 on success, non-zero on failure (implementation-defined).
 */
int rpcb_set(const char *netid, unsigned int program, unsigned int version,
             const struct sockaddr *addr);

#ifdef __cplusplus
}
#endif

#endif /* ONCRPC_H */
