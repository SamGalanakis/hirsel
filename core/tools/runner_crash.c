/* W2 owns the filesystem in this parent. Server and client run in separate
 * processes; provider calls cross a local socketpair. A W2 cut exits only the
 * server, without cleanup. The next server sees W2's restarted durable image. */
#include "hirsel_core_rt_host.h"
#include "hirsel_core_rt_sites.h"
#include <errno.h>
#include <poll.h>
#include <signal.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

extern void *store_crash_new(uint64_t);
extern void store_crash_phase(void *,uint64_t);
extern HirselCoreHostOutcome store_crash_op(void *,uint8_t,uint8_t,uint64_t,const uint8_t *,size_t,uint8_t *,size_t,size_t);
extern uint64_t store_crash_events(void *);
extern uint8_t store_crash_acked(void *);
extern uint64_t store_crash_floor(void *);
extern void store_crash_sync_failure(void *,uint8_t);
extern uint64_t store_crash_failed_syncs(void *);
extern uint8_t store_crash_crashed(void *);
extern void store_crash_process_exit(void *);
extern void store_crash_signal_exit(void *);
extern void store_crash_free(void *);

typedef struct { uint64_t handle,path_len,len,capacity; uint8_t op,mode; } Request;
typedef struct { HirselCoreHostOutcome out; uint64_t bytes; uint8_t crash; } Response;
typedef struct { pid_t pid; int fd,finished,status; } Child;
static int rpc;
static unsigned port;
static char saved_token[33]="-";
static int freeze_sync, sync_parked;
static void transfer(int fd,void *data,size_t len,int writing) {
    uint8_t *p=data;
    while(len) {
        ssize_t n=writing?write(fd,p,len):read(fd,p,len);
        if(n<0 && errno==EINTR) continue;
        if(n<=0) _exit(121);
        p+=n;len-=(size_t)n;
    }
}
static HirselCoreWait ready(void *s,HirselCoreHostRequest *r,uint64_t token) {
    (void)s;(void)token;
    if(freeze_sync && r->operation==16) {
        sync_parked=1;
        return (HirselCoreWait){.kind=2};
    }
    return (HirselCoreWait){0};
}
static HirselCoreStep issue(void *s,HirselCoreHostRequest *r,uint64_t token) {
    (void)s;(void)token;
    if(r->operation==12 && r->buffer.len==6 && !memcmp(r->buffer.data,"FREEZE",6)) freeze_sync=1;
    if(r->operation==12 && r->buffer.len==7 && !memcmp(r->buffer.data,"FROZEN?",7) && !sync_parked)
        return (HirselCoreStep){.outcome.error.tag=1};
    Request q={r->handle,r->path_len,r->buffer.len,r->buffer.capacity,r->operation,r->mode};
    transfer(rpc,&q,sizeof q,1);
    transfer(rpc,(void *)r->path,r->path_len,1);
    transfer(rpc,r->buffer.data,r->buffer.len,1);
    Response response;
    transfer(rpc,&response,sizeof response,0);
    if(response.crash) _exit(99);
    if(response.bytes) {
        if(response.bytes>r->buffer.capacity-r->buffer.len) abort();
        transfer(rpc,r->buffer.data+r->buffer.len,response.bytes,0);
        r->buffer.len+=response.bytes;
    }
    return (HirselCoreStep){.outcome=response.out};
}
static HirselCoreStep resume(void *s,HirselCoreHostRequest *r,uint64_t token) {
    (void)s;(void)r;(void)token;abort();
}
static HirselCoreHostOutcome cancel(void *s,HirselCoreHostRequest *r,uint64_t token,uint8_t why) {
    (void)s;(void)token;(void)why;
    if(freeze_sync && r->operation==16) return (HirselCoreHostOutcome){.error.tag=4};
    abort();
}
static HirselCoreHostOutcome deliver(void *s,HirselCoreHostRequest *r,HirselCoreHostOutcome out) {
    (void)s;(void)r;return out;
}
static Child launch(const HirselCoreOsConfig *input,const HirselCoreSourceTable *sources,HirselCoreEntry entry,const char **args,size_t argc,int inherited,int peer_in,int peer_out) {
    int pair[2];if(socketpair(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0,pair)) abort();
    pid_t pid=fork();if(pid<0) abort();
    if(!pid) {
        close(pair[0]);if(inherited>=0) close(inherited);rpc=pair[1];
        close(peer_out);
        if(peer_in>=0 && peer_in!=STDIN_FILENO) {
            if(dup2(peer_in,STDIN_FILENO)<0) abort();
            close(peer_in);
        }
        HirselCoreProvider provider={NULL,ready,issue,resume,cancel,deliver};
        HirselCoreOpenProcess open=hirsel_core_os_process_new(input);
        if(!open.process) _exit(103);
        hirsel_core_os_process_set_sources(open.process,sources);
        HirselCoreProcessConfig cfg=*hirsel_core_os_process_config(open.process);
        HirselCoreEntryGrant caps[16];
        if(cfg.capabilities_len>16) abort();
        memcpy(caps,cfg.capabilities,cfg.capabilities_len*sizeof *caps);
        for(size_t i=0;i<cfg.capabilities_len;i++) {
            if(caps[i].kind==0 || caps[i].kind==3 || caps[i].kind==4) {
                caps[i].provider=&provider;caps[i].handle=caps[i].kind==0?7:0;
            }
        }
        HirselCoreBuffer argv[6];if(argc>6) abort();
        for(size_t i=0;i<argc;i++) argv[i]=(HirselCoreBuffer){(uint8_t *)args[i],strlen(args[i]),0};
        cfg.capabilities=caps;cfg.args=argv;cfg.args_len=argc;
        if(argc>=2 && !strncmp(args[1],"runner-unavailable-",19)) cfg.launch=NULL;
        uint8_t status=hirsel_core_run(&cfg,entry);
        hirsel_core_os_process_free(open.process);
        close(rpc);_exit(status);
    }
    close(pair[1]);return (Child){.pid=pid,.fd=pair[0]};
}
static void signal_child(const Child *child,int signal) {
    /* Only this fork's PID is eligible. Check its comm immediately before
     * signaling; it must still be this executable, never a parent/helper. */
    char command[80],comm[80],own[80];
    snprintf(command,sizeof command,"ps -o comm= -p %ld",(long)child->pid);
    FILE *p=popen(command,"r"),*me=fopen("/proc/self/comm","r");
    if(!p || !me || !fgets(comm,sizeof comm,p) || !fgets(own,sizeof own,me)) abort();
    int status=pclose(p);fclose(me);
    if(status || strcmp(comm,own)) abort();
    if(kill(child->pid,signal) && errno!=ESRCH) abort();
}
static void stop(Child *child) {
    if(child->finished) return;
    pid_t ended=waitpid(child->pid,&child->status,WNOHANG);
    if(ended==0) {
        signal_child(child,SIGKILL);
        if(waitpid(child->pid,&child->status,0)!=child->pid) abort();
    } else if(ended!=child->pid) abort();
    child->finished=1;if(child->fd>=0) close(child->fd);child->fd=-1;
}
static void service(void *state,Child *child,int server,int phase,int stop_case,int peer_out) {
    Request q;ssize_t first=read(child->fd,&q,sizeof q);
    if(first<0 && errno==EINTR) return;
    if(first==0 || (first<0 && errno==ECONNRESET)) { close(child->fd);child->fd=-1;return; }
    if(first<0) abort();
    if((size_t)first<sizeof q) transfer(child->fd,(uint8_t *)&q+first,sizeof q-(size_t)first,0);
    size_t capacity=q.capacity>q.len?q.capacity:q.len;
    uint8_t *path=malloc(q.path_len?q.path_len:1),*data=calloc(capacity?capacity:1,1);
    if(!path || !data) abort();
    transfer(child->fd,path,q.path_len,0);transfer(child->fd,data,q.len,0);
    int second=stop_case && server && phase==1 && q.op==12 && q.len==6 && !memcmp(data,"SECOND",6);
    if(q.op==12) {
        if(stop_case && server && phase==1 &&
           ((q.len==7 && !memcmp(data,"FROZEN?",7)) || (q.len==5 && !memcmp(data,"EARLY",5))))
            signal_child(child,SIGTERM);
        if(q.len>5 && !memcmp(data,"PORT ",5)) {
            char text[32]={0};if(q.len-5>=sizeof text) abort();memcpy(text,data+5,q.len-5);port=(unsigned)strtoul(text,NULL,10);
        } else if(!server && q.len>5 && !memcmp(data,"PEER ",5)) {
            char text[16]={0},*end;
            if(q.len-5>=sizeof text) abort();
            memcpy(text,data+5,q.len-5);
            unsigned long peer=strtoul(text,&end,10);
            if(*end || peer==0 || peer>65535) abort();
            uint8_t bytes[]={peer&255,peer>>8};
            ssize_t sent;
            do { sent=send(peer_out,bytes,sizeof bytes,MSG_NOSIGNAL); } while(sent<0 && errno==EINTR);
            if(sent!=(ssize_t)sizeof bytes && !(sent<0 && (errno==EPIPE || errno==ECONNRESET))) abort();
        } else if(q.len==38 && !memcmp(data,"TOKEN ",6)) {
            memcpy(saved_token,data+6,32);saved_token[32]=0;
        }
    }
    HirselCoreHostOutcome out=store_crash_op(state,q.op,q.mode,q.handle,path,q.path_len,data,q.len,q.capacity);
    uint64_t bytes=!out.error.tag?((q.op==1 || q.op==37)?out.value:q.op==18?out.written:0):0;
    Response response={out,bytes,(uint8_t)(server && phase==1 && store_crash_crashed(state))};
    if(second) {
        signal_child(child,SIGTERM);
        ssize_t sent=send(child->fd,&response,sizeof response,MSG_NOSIGNAL);
        if(sent!=(ssize_t)sizeof response && !(sent<0 && (errno==EPIPE || errno==ECONNRESET))) abort();
    } else transfer(child->fd,&response,sizeof response,1);
    if(bytes) transfer(child->fd,data+q.len,bytes,1);
    free(path);free(data);
}
static int phase(void *state,const HirselCoreOsConfig *input,const HirselCoreSourceTable *sources,HirselCoreEntry entry,const char *scenario,int number) {
    static const char *modes[]={"init","write","read"};
    port=0;
    char floor[32];snprintf(floor,sizeof floor,"%llu",(unsigned long long)store_crash_floor(state));
    const char *server_args[]={modes[number],scenario,store_crash_acked(state)?"new":"prefix",floor,saved_token};
    int twice=!strcmp(scenario,"runner-stop-twice");
    int deadline_case=!strcmp(scenario,"runner-deadline");
    int stop_case=deadline_case || twice || !strcmp(scenario,"runner-stop-early");
    int peers[2];
    if(socketpair(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0,peers)) abort();
    Child server=launch(input,sources,entry,server_args,stop_case?5:4,-1,peers[0],peers[1]);
    close(peers[0]);
    Child client={.fd=-1,.finished=1};
    struct timespec start;
    if(deadline_case && clock_gettime(CLOCK_MONOTONIC,&start)) abort();
    while(!server.finished || !client.finished) {
        if(deadline_case) {
            struct timespec now;
            if(clock_gettime(CLOCK_MONOTONIC,&now)) abort();
            if(now.tv_sec-start.tv_sec>=8) {
                stop(&server);
                close(peers[1]);
                store_crash_signal_exit(state);
                fprintf(stderr,"%s: teardown did not finish\n",scenario);
                return 120;
            }
        }
        if(port && client.pid==0) {
            char address[16];snprintf(address,sizeof address,"%u",port);
            const char *client_args[]={"client",scenario,modes[number],address,saved_token};
            client=launch(input,sources,entry,client_args,5,server.fd,-1,peers[1]);
        }
        Child *children[]={&server,&client};
        for(size_t i=0;i<2;i++) {
            Child *c=children[i];if(c->finished) continue;
            pid_t ended=waitpid(c->pid,&c->status,WNOHANG);
            if(ended==c->pid) {c->finished=1;if(c->fd>=0) close(c->fd);c->fd=-1;}
            else if(ended<0) abort();
        }
        if(client.pid && client.finished && client.status) {stop(&server);close(peers[1]);return 120;}
        if(server.finished && client.pid==0) break;
        struct pollfd fds[2]={{server.fd,POLLIN,0},{client.fd,POLLIN,0}};
        if(server.finished && client.finished) break;
        int count=poll(fds,2,100);if(count<0 && errno==EINTR) continue;if(count<0) abort();
        if(fds[0].revents) service(state,&server,1,number,stop_case,peers[1]);
        if(fds[1].revents) service(state,&client,0,number,stop_case,peers[1]);
    }
    close(peers[1]);
    if(store_crash_crashed(state)) store_crash_process_exit(state);
    if(twice && number==1) {
        if(WIFSIGNALED(server.status) && WTERMSIG(server.status)==SIGTERM) {
            store_crash_signal_exit(state);
            return 0;
        }
        fprintf(stderr,"second SIGTERM did not crash the place: wait status %d\n",server.status);
        return 120;
    }
    int expected=0;
    if(WIFEXITED(server.status)) {
        int code=WEXITSTATUS(server.status);
        expected=deadline_case && number==1 ? code==1 : (code==0 || (number==1 && code==99));
    }
    if(!expected) {
        fprintf(stderr,"server phase %s: wait status %d\n",modes[number],server.status);return 120;
    }
    return 0;
}
uint8_t __wrap_hirsel_core_os_run_with_sources(const HirselCoreOsConfig *input,const HirselCoreView *names,const HirselCoreSourceTable *sources,HirselCoreEntry entry) {
    (void)names;if(input->args_len!=2 || input->parameters_len>16) return 120;
    char scenario[64]={0},cut_text[32]={0};
    if(input->args[0].len>=sizeof scenario || input->args[1].len>=sizeof cut_text) return 120;
    memcpy(scenario,input->args[0].data,input->args[0].len);memcpy(cut_text,input->args[1].data,input->args[1].len);
    int uncut=!strcmp(cut_text,"count"),poison=!strcmp(scenario,"poison-segment");
    uint64_t cut=uncut||poison?UINT64_MAX:strtoull(cut_text,NULL,10);
    void *state=store_crash_new(cut);store_crash_sync_failure(state,poison?7:0);
    uint8_t status=0;
    int early=!strcmp(scenario,"runner-stop-early"),twice=!strcmp(scenario,"runner-stop-twice");
    for(int n=early?1:0;n<(early || twice?2:3);n++) {
        store_crash_phase(state,(uint64_t)n);
        if(phase(state,input,sources,entry,scenario,n)) {status=120;break;}
        if(n==1 && poison && store_crash_failed_syncs(state)!=1) {fprintf(stderr,"named sync failure was not reached\n");status=120;break;}
        if(n==1 && uncut && !early && !twice && !store_crash_acked(state)) {fprintf(stderr,"uncut run did not acknowledge its checkpoint\n");status=120;break;}
    }
    if(!status) printf("cuts %llu\n",(unsigned long long)store_crash_events(state));
    store_crash_free(state);return status;
}
