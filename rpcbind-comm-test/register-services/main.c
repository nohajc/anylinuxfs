#include <netinet/in.h>
#include <nfs/rpcv2.h>
#include <sys/socket.h>
#include <sys/types.h>
#include <sys/un.h>

#include "common.h"
#include "oncrpc.h"

#include <stdio.h>
#include <string.h>

#define NFSUDPPORT 2049
#define NFSUDP6PORT NFSUDPPORT
#define NFSTCPPORT NFSUDPPORT
#define NFSTCP6PORT NFSTCPPORT

#define MOUNTUDPPORT 32767
#define MOUNTUDP6PORT MOUNTUDPPORT
#define MOUNTTCPPORT MOUNTUDPPORT
#define MOUNTTCP6PORT MOUNTTCPPORT

/*
 * register NFS and MOUNT services with portmap
 * If statd_only is true, register RPCPROG_STAT v1 on UDP 710 and TCP 904
 */
static void register_services(int unset_only, int statd_only) {
  struct sockaddr_storage ss, ss6, ssun;
  struct sockaddr *sa = (struct sockaddr *)&ss;
  struct sockaddr *sa6 = (struct sockaddr *)&ss6;
  struct sockaddr *saun = (struct sockaddr *)&ssun;
  struct sockaddr_in *sin = (struct sockaddr_in *)&ss;
  struct sockaddr_in6 *sin6 = (struct sockaddr_in6 *)&ss6;
  struct sockaddr_un *sun = (struct sockaddr_un *)&ssun;
  int errcnt;

  sin->sin_family = AF_INET;
  sin->sin_addr.s_addr = INADDR_ANY;
  sin->sin_len = sizeof(*sin);
  sin6->sin6_family = AF_INET6;
  sin6->sin6_addr = in6addr_any;
  sin6->sin6_len = sizeof(*sin6);
  sun->sun_family = AF_LOCAL;
  sun->sun_len = sizeof(*sun);

  /* nfsd */
  rpcb_unset(NULL, RPCPROG_NFS, 3);
  rpcb_unset(NULL, RPCPROG_NFS, 4);

  /* mountd */
  rpcb_unset(NULL, RPCPROG_MNT, 1);
  rpcb_unset(NULL, RPCPROG_MNT, 2);
  rpcb_unset(NULL, RPCPROG_MNT, 3);

  /* statd: if we're going to operate on statd only, unset it here too */
  if (statd_only) {
    rpcb_unset(NULL, RPCPROG_STAT, 1);
  }

  /* If called with -u, only perform the rpcb_unset calls above and return. */
  if (unset_only) {
    return;
  }

  /* If -s was specified, register statd on the fixed ports and skip other
   * services */
  if (statd_only) {
    errcnt = 0;

    /* UDP: port 710 */
    sin->sin_port = htons(710);
    if (!rpcb_set("udp", RPCPROG_STAT, 1, sa)) {
      errcnt++;
    }

    if (!rpcb_set("udp6", RPCPROG_STAT, 1, sa)) {
      errcnt++;
    }

    /* TCP: port 904 */
    sin->sin_port = htons(904);
    if (!rpcb_set("tcp", RPCPROG_STAT, 1, sa)) {
      errcnt++;
    }

    if (!rpcb_set("tcp6", RPCPROG_STAT, 1, sa6)) {
      errcnt++;
    }

    if (errcnt) {
      fprintf(stderr, "couldn't register STAT service.\n");
    }

    return;
  }

  /* nfsd */
  errcnt = 0;

  sin->sin_port = htons(NFSUDPPORT);
  if (!rpcb_set("udp", RPCPROG_NFS, 2, sa)) {
    errcnt++;
  }
  if (!rpcb_set("udp", RPCPROG_NFS, 3, sa)) {
    errcnt++;
  }

  sin6->sin6_port = htons(NFSUDP6PORT);
  if (!rpcb_set("udp6", RPCPROG_NFS, 2, sa6)) {
    errcnt++;
  }
  if (!rpcb_set("udp6", RPCPROG_NFS, 3, sa6)) {
    errcnt++;
  }

  if (errcnt) {
    fprintf(stderr, "couldn't register NFS/UDP service.\n");
  }

  errcnt = 0;

  sin->sin_port = htons(NFSTCPPORT);
  if (!rpcb_set("tcp", RPCPROG_NFS, 2, sa)) {
    errcnt++;
  }
  if (!rpcb_set("tcp", RPCPROG_NFS, 3, sa)) {
    errcnt++;
  }

  sin6->sin6_port = htons(NFSTCP6PORT);
  if (!rpcb_set("tcp6", RPCPROG_NFS, 2, sa6)) {
    errcnt++;
  }
  if (!rpcb_set("tcp6", RPCPROG_NFS, 3, sa6)) {
    errcnt++;
  }

  if (errcnt) {
    fprintf(stderr, "couldn't register NFS/TCP service.\n");
  }

#ifdef _PATH_NFSD_TICLTS_SOCK
  /* XXX if (config.ticlts?) */
  {
    errcnt = 0;
    strlcpy(sun->sun_path, _PATH_NFSD_TICLTS_SOCK, sizeof(sun->sun_path));
    if (!rpcb_set("ticotsord", RPCPROG_NFS, 2, saun)) {
      errcnt++;
    }
    if (!rpcb_set("ticlts", RPCPROG_NFS, 3, saun)) {
      errcnt++;
    }
    if (errcnt) {
      fprintf(stderr, "coundn't register NFS/TICLTS service.\n");
    }
  }
#endif

#ifdef _PATH_NFSD_TICOTSORD_SOCK
  /* XXX if (config.ticotsord?) */
  {
    errcnt = 0;
    strlcpy(sun->sun_path, _PATH_NFSD_TICOTSORD_SOCK, sizeof(sun->sun_path));
    if (!rpcb_set("ticotsord", RPCPROG_NFS, 2, saun)) {
      errcnt++;
    }
    if (!rpcb_set("ticotsord", RPCPROG_NFS, 3, saun)) {
      errcnt++;
    }
    if (errcnt) {
      fprintf(stderr, "coundn't register NFS/TICOTSORD service.\n");
    }
  }
#endif

  /* mountd */
  errcnt = 0;

  sin->sin_port = htons(MOUNTUDPPORT);
  if (!rpcb_set("udp", RPCPROG_MNT, 1, sa)) {
    errcnt++;
  }
  if (!rpcb_set("udp", RPCPROG_MNT, 3, sa)) {
    errcnt++;
  }

  sin6->sin6_port = htons(MOUNTUDP6PORT);
  if (!rpcb_set("udp6", RPCPROG_MNT, 1, sa6)) {
    errcnt++;
  }
  if (!rpcb_set("udp6", RPCPROG_MNT, 3, sa6)) {
    errcnt++;
  }

  if (errcnt) {
    fprintf(stderr, "couldn't register MOUNT/UDP service.\n");
  }

  errcnt = 0;

  sin->sin_port = htons(MOUNTTCPPORT);
  if (!rpcb_set("tcp", RPCPROG_MNT, 1, sa)) {
    errcnt++;
  }
  if (!rpcb_set("tcp", RPCPROG_MNT, 3, sa)) {
    errcnt++;
  }

  sin6->sin6_port = htons(MOUNTTCP6PORT);
  if (!rpcb_set("tcp6", RPCPROG_MNT, 1, sa6)) {
    errcnt++;
  }
  if (!rpcb_set("tcp6", RPCPROG_MNT, 3, sa6)) {
    errcnt++;
  }

  if (errcnt) {
    fprintf(stderr, "couldn't register MOUNT/TCP service.\n");
  }

#ifdef _PATH_MOUNTD_TICLTS_SOCK
  /*XXX if (config.ticlts?) */
  {
    strlcpy(sun->sun_path, _PATH_MOUNTD_TICLTS_SOCK, sizeof(sun->sun_path));
    if (!rpcb_set("ticlts", RPCPROG_MNT, 1, &sun)) {
      errcnt++;
    }
    if (!rpcb_set("ticlts", RPCPROG_MNT, 3, &sun)) {
      errcnt++;
    }
    if (errcnt) {
      fprintf(stderr, "coundn't register NFS/TICLTS service.\n");
    }
  }
#endif

#ifdef _PATH_MOUNTD_TICOTSORD_SOCK
  /*XXX if (config.ticotsord?) */
  {
    strlcpy(sun->sun_path, _PATH_MOUNTD_TICOTSORD_SOCK, sizeof(sun->sun_path));
    if (!rpcb_set("ticotsord", RPCPROG_MNT, 1, (const struct sockaddr *)sun)) {
      errcnt++;
    }
    if (!rpcb_set("ticotsord", RPCPROG_MNT, 3, (const struct sockaddr *)sun)) {
      errcnt++;
    }
    if (errcnt) {
      fprintf(stderr, "coundn't register NFS/TICOTSORD service.\n");
    }
  }
#endif
}

int main(int argc, char **argv) {
  int unset_only = 0;
  int statd_only = 0;

  for (int i = 1; i < argc; i++) {
    if (strcmp(argv[i], "-u") == 0) {
      unset_only = 1;
    } else if (strcmp(argv[i], "-s") == 0) {
      statd_only = 1;
    }
  }

  register_services(unset_only, statd_only);
  return 0;
}
