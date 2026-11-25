#include <arpa/inet.h>
#include <errno.h>
#include <libproc.h>
#include <netinet/tcp_fsm.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>

struct owner {
    uint64_t unique_id;
    uint64_t start_time;
    int32_t pid;
    int32_t result;
    char name[MAXCOMLEN * 2];
};

/* XNU's PROC_PIDT_BSDINFOWITHUNIQID returns both IDs in one call. */
#define BSD_INFO_WITH_UNIQUE_ID 18
struct bsd_with_unique_id {
    struct proc_bsdinfo bsd;
    uint8_t uuid[16];
    uint64_t unique_id;
    uint64_t parent_unique_id;
    uint64_t reserved[3];
};

struct peer {
    uint32_t address;
    uint16_t port;
};

static int matches(int pid, int fd, uint16_t proxy_port, struct peer *peer) {
    struct socket_fdinfo socket;
    int size = proc_pidfdinfo(pid, fd, PROC_PIDFDSOCKETINFO, &socket, sizeof(socket));
    if (size != sizeof(socket) || socket.psi.soi_kind != SOCKINFO_TCP ||
        socket.psi.soi_family != AF_INET) {
        return 0;
    }
    const struct tcp_sockinfo *tcp = &socket.psi.soi_proto.pri_tcp;
    const struct in_sockinfo *ip = &tcp->tcpsi_ini;
    if (tcp->tcpsi_state != TCPS_ESTABLISHED ||
        ip->insi_faddr.ina_46.i46a_addr4.s_addr != htonl(INADDR_LOOPBACK) ||
        ntohs(ip->insi_fport) != proxy_port) {
        return 0;
    }
    peer->address = ip->insi_laddr.ina_46.i46a_addr4.s_addr;
    peer->port = ntohs(ip->insi_lport);
    return 1;
}

void zentop_socket_owners(uint16_t proxy_port, const struct peer *peers,
                          struct owner *owners, size_t count) {
    int capacity = proc_listallpids(NULL, 0);
    if (capacity <= 0) {
        return;
    }
    capacity += 256;
    int *pids = calloc(capacity, sizeof(*pids));
    if (!pids) {
        return;
    }
    int denied = 0;
    struct proc_fdinfo *fds = NULL;
    int fd_capacity = 0;
    int pid_count = proc_listallpids(pids, capacity * sizeof(*pids));
    for (int i = 0; i < pid_count; i++) {
        int pid = pids[i];
        if (pid <= 0) {
            continue;
        }
        int size = proc_pidinfo(pid, PROC_PIDLISTFDS, 0, NULL, 0);
        if (size <= 0) {
            denied |= errno == EPERM || errno == EACCES;
            continue;
        }
        if (size > fd_capacity) {
            int next_capacity = size + 32 * sizeof(*fds);
            void *next = realloc(fds, next_capacity);
            if (!next) {
                break;
            }
            fds = next;
            fd_capacity = next_capacity;
        }
        size = proc_pidinfo(pid, PROC_PIDLISTFDS, 0, fds, fd_capacity);
        denied |= size <= 0 && (errno == EPERM || errno == EACCES);
        if (size <= 0) {
            continue;
        }
        for (int j = 0; j < size / (int)sizeof(*fds); j++) {
            struct peer peer;
            if (fds[j].proc_fdtype != PROX_FDTYPE_SOCKET ||
                !matches(pid, fds[j].proc_fd, proxy_port, &peer)) {
                continue;
            }
            for (size_t k = 0; k < count; k++) {
                if (peers[k].address != peer.address || peers[k].port != peer.port ||
                    owners[k].result == 2 || owners[k].pid == pid) {
                    continue;
                }
                if (owners[k].pid != 0) {
                    owners[k].result = 2;
                    continue;
                }
                struct bsd_with_unique_id info;
                owners[k].pid = pid;
                if (proc_pidinfo(pid, BSD_INFO_WITH_UNIQUE_ID, 0, &info, sizeof(info)) !=
                        sizeof(info) ||
                    !matches(pid, fds[j].proc_fd, proxy_port, &peer) ||
                    peer.address != peers[k].address || peer.port != peers[k].port) {
                    owners[k].result = 3;
                    continue;
                }
                owners[k].result = 1;
                owners[k].unique_id = info.unique_id;
                owners[k].start_time = info.bsd.pbi_start_tvsec;
                memcpy(owners[k].name, info.bsd.pbi_name, sizeof(owners[k].name));
                if (!owners[k].name[0]) {
                    memcpy(owners[k].name, info.bsd.pbi_comm, sizeof(info.bsd.pbi_comm));
                }
            }
        }
    }
    for (size_t k = 0; k < count; k++) {
        if (owners[k].result == 0 && denied) {
            owners[k].result = 4;
        }
    }
    free(fds);
    free(pids);
}

/* Identity of one process, for the launched-family client-access policy. */
struct identity {
    uint64_t unique_id;
    uint64_t parent_unique_id;
    uint64_t start_sec;
    uint64_t start_usec;
    int32_t pid;
    int32_t ppid;
};

int zentop_process_identity(int pid, struct identity *out) {
    struct bsd_with_unique_id info;
    if (pid <= 0 ||
        proc_pidinfo(pid, BSD_INFO_WITH_UNIQUE_ID, 0, &info, sizeof(info)) != sizeof(info)) {
        return 0;
    }
    out->unique_id = info.unique_id;
    out->parent_unique_id = info.parent_unique_id;
    out->start_sec = info.bsd.pbi_start_tvsec;
    out->start_usec = info.bsd.pbi_start_tvusec;
    out->pid = (int32_t)info.bsd.pbi_pid;
    out->ppid = (int32_t)info.bsd.pbi_ppid;
    return 1;
}

int zentop_process_path(int pid, char *buffer, uint32_t size) {
    if (pid <= 0) {
        return 0;
    }
    return proc_pidpath(pid, buffer, size);
}

/* Exported by libSystem since macOS 10.14; not declared in a public header. */
extern pid_t responsibility_get_pid_responsible_for_pid(pid_t pid);

int zentop_responsible_pid(int pid) {
    return pid > 0 ? responsibility_get_pid_responsible_for_pid(pid) : -1;
}
