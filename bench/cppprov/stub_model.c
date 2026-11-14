/* stub_model N DT : stand-in for a Simulink Coder model + modelHooks.c (ww-1df.42), plumbing only, no solver.
 * Speaks the same contract as sceptre-bennu src/bennu/executables/bennu-simulink-provider/solver/modelhooks/modelHooks.{c,h}, so the unmodified
 * C++ bennu-simulink-provider attaches to it: SysV shm key 10610 (point count as text) and 10613 (N slots of 256 bytes, "tag:TYPE:value"),
 * POSIX semaphores publish_sem / updates_sem (created locked, posted after init), FIFO /tmp/updates_fifo (256-byte "tag:TYPE:value" messages).
 * The "model": N breaker-like points brk-0000.active .. (BOOLEAN, start closed = 1). Every DT seconds (fixed step, absolute clock like a Simulink
 * fixed-step solver): step() as in modelHooks.c, in the same order: publishState under publish_sem, then applyUpdates under updates_sem, so a write
 * becomes visible in shm one step after it was applied. A write to a point sets its state (the echo); the status published next step is that value.
 * Each applied update prints `Applied: t_apply_ns= t_solved_ns= tag= val=` (the shape bench/control_join.py joins; t_solved_ns = end of that step).
 * Not modelled: the solver's own compute time, and modelHooks.c's per-point linear search of the CAPI signal table (getModelSignalValue). */
#include <errno.h>
#include <fcntl.h>
#include <semaphore.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/ipc.h>
#include <sys/shm.h>
#include <sys/stat.h>
#include <time.h>
#include <unistd.h>

#define MAX_MSG_LEN 256
#define NUM_KEY 10610
#define PTS_KEY 10613
#define UPDATES_FIFO "/tmp/updates_fifo"

static unsigned long long now_ns(void) {
    struct timespec t;
    clock_gettime(CLOCK_REALTIME, &t);
    return t.tv_sec * 1000000000ULL + t.tv_nsec;
}

static char *attach(key_t key, int size) {
    int id = shmget(key, size, IPC_CREAT | 0666);
    char *p = id < 0 ? (char *)-1 : shmat(id, NULL, 0);
    if (p == (char *)-1) { printf("Fatal: shared memory %d: %s\n", key, strerror(errno)); exit(1); }
    memset(p, 0, size);
    return p;
}

int main(int argc, char **argv) {
    int n = argc > 1 ? atoi(argv[1]) : 100;
    double dt = argc > 2 ? atof(argv[2]) : 0.05;
    setbuf(stdout, NULL);
    char *state = malloc(n);
    memset(state, 1, n);
    sem_unlink("publish_sem"); sem_unlink("updates_sem");
    sem_t *pub = sem_open("publish_sem", O_CREAT, 0644, 0), *upd = sem_open("updates_sem", O_CREAT, 0644, 0);
    if (pub == SEM_FAILED || upd == SEM_FAILED) { printf("Fatal: semaphores\n"); return 1; }
    unlink(UPDATES_FIFO);
    if (mkfifo(UPDATES_FIFO, 0666) == -1) { printf("Fatal: mkfifo\n"); return 1; }
    int fifo = open(UPDATES_FIFO, O_RDONLY | O_NONBLOCK);
    sprintf(attach(NUM_KEY, sizeof(unsigned int)), "%u", (unsigned)n);
    char *shm = attach(PTS_KEY, n * MAX_MSG_LEN);
    printf("Info: stub model, %d points, step %.3f s\n", n, dt);
    for (int i = 0; i < n; i++) sprintf(shm + i * MAX_MSG_LEN, "brk-%04d.active:BOOLEAN:%d", i, state[i]);
    sem_post(pub); sem_post(upd);

    struct timespec next;
    clock_gettime(CLOCK_MONOTONIC, &next);
    char msg[MAX_MSG_LEN];
    for (;;) {
        next.tv_nsec += (long)(dt * 1e9);
        while (next.tv_nsec >= 1000000000L) { next.tv_nsec -= 1000000000L; next.tv_sec++; }
        clock_nanosleep(CLOCK_MONOTONIC, TIMER_ABSTIME, &next, NULL);

        sem_wait(pub);   /* publishState */
        for (int i = 0; i < n; i++) sprintf(shm + i * MAX_MSG_LEN, "brk-%04d.active:BOOLEAN:%d", i, state[i]);
        sem_post(pub);

        sem_wait(upd);   /* applyUpdates */
        unsigned long long t_apply = now_ns(), seen = 0;
        static char tags[4096][48]; static int vals[4096]; /* stamps are printed after the step; the FIFO holds at most 256 messages (64 KiB) */
        while (read(fifo, msg, MAX_MSG_LEN) > 0) {
            char *tag = strtok(msg, ":"), *type = strtok(NULL, ":"), *val = strtok(NULL, ":");
            (void)type;
            int i = (tag && !strncmp(tag, "brk-", 4)) ? atoi(tag + 4) : -1;
            if (i < 0 || i >= n || !val) continue;
            state[i] = (char)strtol(val, NULL, 2);   /* BOOLEAN branch of updateModelParameterValue */
            if (seen < 4096) { snprintf(tags[seen], sizeof tags[0], "%s", tag); vals[seen] = state[i]; }
            seen++;
        }
        sem_post(upd);
        if (seen) {
            unsigned long long t_solved = now_ns();
            for (unsigned long long k = 0; k < seen && k < 4096; k++)
                printf("Applied: t_apply_ns=%llu t_solved_ns=%llu tag=%s val=%d\n", t_apply, t_solved, tags[k], vals[k]);
        }
    }
}
