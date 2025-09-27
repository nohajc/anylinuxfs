/*
 * Copyright (c) 1999-2010 Apple Inc. All rights reserved.
 *
 * @APPLE_LICENSE_HEADER_START@
 *
 * This file contains Original Code and/or Modifications of Original Code
 * as defined in and that are subject to the Apple Public Source License
 * Version 2.0 (the 'License'). You may not use this file except in
 * compliance with the License. Please obtain a copy of the License at
 * http://www.opensource.apple.com/apsl/ and read it before using this
 * file.
 *
 * The Original Code and all software distributed under the License are
 * distributed on an 'AS IS' basis, WITHOUT WARRANTY OF ANY KIND, EITHER
 * EXPRESS OR IMPLIED, AND APPLE HEREBY DISCLAIMS ALL SUCH WARRANTIES,
 * INCLUDING WITHOUT LIMITATION, ANY WARRANTIES OF MERCHANTABILITY,
 * FITNESS FOR A PARTICULAR PURPOSE, QUIET ENJOYMENT OR NON-INFRINGEMENT.
 * Please see the License for the specific language governing rights and
 * limitations under the License.
 *
 * @APPLE_LICENSE_HEADER_END@
 */

#include <sys/param.h>

/*
 * structure holding NFS server config values
 */
struct nfs_conf_server {
	int async;                      /* sysctl */
	int bonjour;                    /* nfsd */
	int bonjour_local_domain_only;  /* nfsd */
	int export_hash_size;           /* sysctl */
	int fsevents;                   /* sysctl */
	int mount_port;                 /* mountd */
	int mount_regular_files;        /* mountd */
	int mount_require_resv_port;    /* mountd */
	int nfsd_threads;               /* nfsd */
	int port;                       /* nfsd */
	int materialize_dataless_files; /* nfsd */
	int reqcache_size;              /* sysctl */
	int request_queue_length;       /* sysctl */
	int require_resv_port;          /* sysctl */
	int tcp;                        /* nfsd/mountd */
	int udp;                        /* nfsd/mountd */
	int user_stats;                 /* sysctl */
	int verbose;                    /* nfsd/mountd */
	int wg_delay;                   /* sysctl */
	int wg_delay_v3;                /* sysctl */
};

/*
 * verbose log levels:
 * 0 LOG_WARNING and higher
 * 1 LOG_NOTICE and higher
 * 2 LOG_INFO and higher
 * 3 LOG_DEBUG and higher
 * 4+ more LOG_DEBUG
 */
#define LOG_LEVEL       (LOG_WARNING + MIN(config.verbose, 3))
#undef DEBUG
#define DEBUG(L, ...) \
	do { \
	        if ((L) <= (config.verbose - 2)) \
	                SYSLOG(LOG_NOTICE, __VA_ARGS__); \
	} while (0)
#define log SYSLOG
void SYSLOG(int, const char *, ...) __attribute__((format(printf, 2, 3)));

void set_thread_sigmask(void);
int sysctl_get(const char *, int *);
int sysctl_set(const char *, int);

void nfsd(void);
void nfsd_start_server_threads(int);

void mountd_init(void);
void mountd(void);
int get_exportlist(void);
int check_for_mount_changes(void);
int clear_export_errors(uint32_t);

pid_t get_nfsd_pid(void);
int nfsd_is_enabled(void);
int nfsd_is_loaded(void);

pid_t get_lockd_pid(void);
pid_t get_rquotad_pid(void);

int nfsd_imp(int argc, char *argv[], const char *conf_path);

#define MAX_NFSD_THREADS_SOFT   192
#define MAX_NFSD_THREADS_HARD   512

/* export options */
#define OP_MAPROOT      0x00000001      /* map root credentials */
#define OP_MAPALL       0x00000002      /* map all credentials */
#define OP_SECFLAV      0x00000004      /* security flavor(s) specified */
#define OP_MASK         0x00000008      /* network mask specified */
#define OP_NET          0x00000010      /* network address specified */
#define OP_MANGLEDNAMES 0x00000020      /* tell the vfs to mangle names that are > 255 bytes */
#define OP_ALLDIRS      0x00000040      /* allow mounting subdirs */
#define OP_READONLY     0x00000080      /* export read-only */
#define OP_32BITCLIENTS 0x00000100      /* use 32-bit directory cookies */
#define OP_FSPATH       0x00000200      /* file system path specified */
#define OP_FSUUID       0x00000400      /* file system UUID specified */
#define OP_OFFLINE      0x00000800      /* export is offline */
#define OP_ONLINE       0x04000000      /* export is online */
#define OP_SHOW         0x08000000      /* show this entry in export list */
#define OP_MISSING      0x10000000      /* export is missing */
#define OP_DEFEXP       0x20000000      /* default export for everyone (else) */
#define OP_ADD          0x40000000      /* tag export for potential addition */
#define OP_DEL          0x80000000      /* tag export for potential deletion */
#define OP_EXOPTMASK    0x100009E3      /* export options mask */
#define OP_EXOPTS(X)    ((X) & OP_EXOPTMASK)

/* globals */
extern pthread_attr_t pattr;
extern const struct nfs_conf_server config_defaults;
extern struct nfs_conf_server config;
extern volatile int gothup, gotterm;
extern char exportsfilepath[MAXPATHLEN];
extern int checkexports;
extern int nfsudpport, nfstcpport;
extern int nfsudp6port, nfstcp6port;
extern int mountudpport, mounttcpport;
extern int mountudp6port, mounttcp6port;
extern time_t recheckexports_until;
extern int recheckexports;
