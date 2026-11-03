/* rinky build options for dropbear (copied into the source tree by
 * scripts/build-dropbear.sh). Anything not set here keeps the value in
 * src/default_options.h of the pinned release.
 *
 * The ssh plugin passes every path on the command line (-r host keys, -D
 * authorized_keys dir, -P pid file, -p port); the defaults below only matter
 * when dropbear runs without those flags. */
#ifndef RINKY_DROPBEAR_LOCALOPTIONS_H
#define RINKY_DROPBEAR_LOCALOPTIONS_H

/* Same port as scripts/deploy.sh and KOReader. */
#define DROPBEAR_DEFPORT "2222"

/* Started by the plugin, never from inetd. */
#define INETD_MODE 0
#define NON_INETD_MODE 1

/* No X server on the Kindle; X11 forwarding was also the subject of the
 * 2026.93 security fix. */
#define DROPBEAR_X11FWD 0

/* No sftp-server binary is shipped. scp (dropbearmulti scp) is the file
 * transfer path; OpenSSH 9+ clients need `scp -O` (legacy scp protocol). */
#define DROPBEAR_SFTPSERVER 0

/* Host keys: ed25519 and ecdsa (nistp256). RSA stays compiled in so users
 * can log in with RSA keys; the plugin loads only ed25519 + ecdsa host keys
 * (-r), so no RSA host key is offered. DSS is off (upstream default). */
#define DROPBEAR_ED25519 1
#define DROPBEAR_ECDSA 1
#define DROPBEAR_RSA 1
#define DROPBEAR_DSS 0
#define ECDSA_DEFAULT_SIZE 256

/* Defaults for -R (create host keys as required) when no -r is given. The
 * rootfs is read-only, so /etc/dropbear cannot be used. */
#define RSA_PRIV_FILENAME "/mnt/us/rinky/ssh/dropbear_rsa_host_key"
#define DSS_PRIV_FILENAME "/mnt/us/rinky/ssh/dropbear_dss_host_key"
#define ECDSA_PRIV_FILENAME "/mnt/us/rinky/ssh/dropbear_ecdsa_host_key"
#define ED25519_PRIV_FILENAME "/mnt/us/rinky/ssh/dropbear_ed25519_host_key"

/* The Kindle has no motd worth showing. */
#define DO_MOTD 0

/* dropbear sets PATH for sessions itself (the server's PATH is not passed
 * on). /mnt/us/rinky/bin comes last so `scp -t` from a client finds the
 * scp wrapper shipped there without shadowing any system command. */
#define DEFAULT_PATH "/usr/bin:/bin:/mnt/us/rinky/bin"
#define DEFAULT_ROOT_PATH "/usr/sbin:/usr/bin:/sbin:/bin:/mnt/us/rinky/bin"

/* Not used on the device (dbclient is not built). */
#define DROPBEAR_PATH_SSH_PROGRAM "/mnt/us/rinky/bin/dbclient"

#endif
