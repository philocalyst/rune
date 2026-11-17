#define _GNU_SOURCE
/* Fixed synthetic probes ONLY. Not a generic launcher. Not executed. */
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/landlock.h>
#include <linux/seccomp.h>
#include <poll.h>
#include <signal.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <time.h>
#include <unistd.h>

#ifndef LANDLOCK_ACCESS_FS_REFER
#define LANDLOCK_ACCESS_FS_REFER (1ULL<<13)
#endif
#ifndef LANDLOCK_ACCESS_FS_TRUNCATE
#define LANDLOCK_ACCESS_FS_TRUNCATE (1ULL<<14)
#endif
#define ALLOW_NR(n) BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,(n),0,1), BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW)
static const char *modes[]={"positive","read-denied","write-denied","truncate-denied",
 "socket-inet","socket-inet6","socket-unix","socket-abstract","socketpair",
 "io-uring","ptrace","pidfd-getfd","inherited-fd-close","inherited-policy","unavailable-policy","inherited-seccomp"};
static double now(void){struct timespec t;if(clock_gettime(CLOCK_MONOTONIC,&t))_exit(90);return t.tv_sec+t.tv_nsec/1e9;}
static int socket_mode(int mode){return mode>=4 && mode<=11;}
static int install_seccomp(void){
#if !defined(__x86_64__) || defined(__ILP32__)
 return -1;
#else
 struct sock_filter code[]={
  BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,arch)),
  BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,AUDIT_ARCH_X86_64,1,0),
  BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS),
  BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,nr)),
  BPF_JUMP(BPF_JMP|BPF_JSET|BPF_K,0x40000000,0,1),
  BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS),
  ALLOW_NR(__NR_read),ALLOW_NR(__NR_write),ALLOW_NR(__NR_close),
  ALLOW_NR(__NR_openat),ALLOW_NR(__NR_fcntl),ALLOW_NR(__NR_ftruncate),
  ALLOW_NR(__NR_exit),ALLOW_NR(__NR_exit_group),
  ALLOW_NR(__NR_clone),ALLOW_NR(__NR_fork),ALLOW_NR(__NR_wait4),
  ALLOW_NR(__NR_rt_sigreturn),ALLOW_NR(__NR_rt_sigprocmask),
  ALLOW_NR(__NR_set_robust_list),
  BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS)
 };
 struct sock_fprog program={.len=(unsigned short)(sizeof(code)/sizeof(code[0])),.filter=code};
 return (int)syscall(__NR_seccomp,SECCOMP_SET_MODE_FILTER,0,&program);
#endif
}
static int install_landlock(const char *allow,const char *denied,int truncate_mode){
 struct landlock_ruleset_attr rules={.handled_access_fs=(1ULL<<15)-1};
 int fd=(int)syscall(__NR_landlock_create_ruleset,&rules,sizeof(rules),0);
 if(fd<0)return -1;
 int directory=open(allow,O_PATH|O_DIRECTORY|O_CLOEXEC|O_NOFOLLOW);
 if(directory<0){close(fd);return -1;}
 struct landlock_path_beneath_attr path={
  .allowed_access=LANDLOCK_ACCESS_FS_READ_FILE|LANDLOCK_ACCESS_FS_READ_DIR|
   LANDLOCK_ACCESS_FS_WRITE_FILE|LANDLOCK_ACCESS_FS_MAKE_REG|
   LANDLOCK_ACCESS_FS_REMOVE_FILE|LANDLOCK_ACCESS_FS_TRUNCATE,
  .parent_fd=directory};
 int result=(int)syscall(__NR_landlock_add_rule,fd,LANDLOCK_RULE_PATH_BENEATH,&path,0);
 close(directory);
 if(result==0 && truncate_mode){
  int canary=open(denied,O_PATH|O_CLOEXEC|O_NOFOLLOW);
  if(canary<0){close(fd);return -1;}
  struct landlock_path_beneath_attr write_only={.allowed_access=LANDLOCK_ACCESS_FS_WRITE_FILE,.parent_fd=canary};
  result=(int)syscall(__NR_landlock_add_rule,fd,LANDLOCK_RULE_PATH_BENEATH,&write_only,0);
  close(canary);
 }
 if(result==0)result=(int)syscall(__NR_landlock_restrict_self,fd,0);
 close(fd);return result;
}
static void mark(unsigned char marker){if(syscall(__NR_write,3,&marker,1)!=1)_exit(91);}
static int denied_open(const char *path,int flags){
 int fd=(int)syscall(__NR_openat,AT_FDCWD,path,flags,0600);
 if(fd>=0){syscall(__NR_close,fd);return 0;}
 return errno==EACCES;
}
static void child(int mode,int pipefd,const char *allow,const char *input,const char *output,const char *denied){
 if(mode==12 && fcntl(10,F_GETFD)<0)_exit(92);
 if(dup2(pipefd,3)!=3)_exit(92);
 if(syscall(__NR_close_range,4U,~0U,0)!=0)_exit(92);
 if(mode==12 && !(fcntl(10,F_GETFD)==-1 && errno==EBADF))_exit(92);
 if(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)!=0)_exit(93);
 if(prctl(PR_GET_NO_NEW_PRIVS,0,0,0,0)!=1)_exit(93);
 if(mode==14){ /* Explicit synthetic install-failure, no ready/operation. */
  struct landlock_ruleset_attr invalid={.handled_access_fs=1ULL<<63};
  int rejected=(int)syscall(__NR_landlock_create_ruleset,&invalid,sizeof(invalid),0);
  if(rejected>=0){close(rejected);_exit(94);} if(errno!=EINVAL)_exit(94); _exit(77);
 }
 if(install_landlock(allow,denied,mode==3)!=0)_exit(93);
 if(install_seccomp()!=0)_exit(93);
 mark(0xa5); /* No incidental libc syscall between this and fixed target below. */
 int ok=0,fd;char bytes[32];
 switch(mode){
 case 0:
  fd=(int)syscall(__NR_openat,AT_FDCWD,input,O_RDONLY|O_NOFOLLOW,0);
  if(fd<0)_exit(95);
  ssize_t n=syscall(__NR_read,fd,bytes,sizeof(bytes));syscall(__NR_close,fd);
  if(n!=13 || memcmp(bytes,"probe-canary\n",13)!=0)_exit(95);
  fd=(int)syscall(__NR_openat,AT_FDCWD,output,O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW,0600);
  if(fd<0)_exit(95);
  ok=syscall(__NR_write,fd,"probe-output\n",13)==13;syscall(__NR_close,fd);break;
 case 1:ok=denied_open(denied,O_RDONLY|O_NOFOLLOW);break;
 case 2:ok=denied_open(denied,O_WRONLY|O_NOFOLLOW);break;
 case 3:
  fd=(int)syscall(__NR_openat,AT_FDCWD,denied,O_WRONLY|O_NOFOLLOW,0);
  if(fd<0)_exit(95); /* WRITE_FILE succeeds; only TRUNCATE is denied. */
  errno=0;ok=syscall(__NR_ftruncate,fd,0)==-1 && errno==EACCES;
  syscall(__NR_close,fd);break;
 case 4:syscall(__NR_socket,AF_INET,SOCK_STREAM,0);break;
 case 5:syscall(__NR_socket,AF_INET6,SOCK_DGRAM,0);break;
 case 6:case 7:syscall(__NR_socket,AF_UNIX,SOCK_STREAM,0);break;
 case 8:{int pair[2];syscall(__NR_socketpair,AF_UNIX,SOCK_STREAM,0,pair);break;}
 case 9:syscall(__NR_io_uring_setup,1,NULL);break;
 case 10:syscall(__NR_ptrace,0,0,0,0);break;
 case 11:syscall(__NR_pidfd_getfd,-1,0,0);break;
 case 12:errno=0;ok=syscall(__NR_fcntl,10,F_GETFD)==-1 && errno==EBADF;break;
 case 13:case 15:{
  /* Raw fork avoids libc atfork handlers and does not exec any payload. */
  pid_t descendant=(pid_t)syscall(__NR_fork);
  if(descendant<0)_exit(96);
  if(descendant==0){
   mark(0xa6);
   if(mode==15){syscall(__NR_socket,AF_UNIX,SOCK_STREAM,0);_exit(97);}
   int result=denied_open(denied,O_RDONLY|O_NOFOLLOW);_exit(result?0:97);
  }
  int status=0;pid_t got=(pid_t)syscall(__NR_wait4,descendant,&status,0,NULL);
  ok=got==descendant && (mode==15 ? (WIFSIGNALED(status)&&WTERMSIG(status)==SIGSYS) : (WIFEXITED(status)&&WEXITSTATUS(status)==0));break;
 }
 default:_exit(98);
 }
 _exit(ok?0:99);
}
static int canary_accessible(const char *path){
 int fd=open(path,O_RDONLY|O_NOFOLLOW|O_CLOEXEC);if(fd<0)return 0;
 struct stat st;char b[14];ssize_t n=read(fd,b,sizeof(b));
 int ok=fstat(fd,&st)==0 && S_ISREG(st.st_mode) && st.st_uid==getuid() && n==13 && !memcmp(b,"probe-canary\n",13);
 close(fd);
 if(!ok)return 0;
 /* Establish ordinary DAC permits writing, without changing canary bytes. */
 fd=open(path,O_WRONLY|O_NOFOLLOW|O_CLOEXEC);
 if(fd<0)return 0;
 int writable=fstat(fd,&st)==0 && S_ISREG(st.st_mode) && st.st_uid==getuid();
 close(fd);return writable;
}
int main(int argc,char **argv){
 if(argc!=4)return 64;
 int mode=-1;for(size_t i=0;i<sizeof(modes)/sizeof(modes[0]);++i)if(!strcmp(argv[1],modes[i]))mode=(int)i;
 if(mode<0)return 64;
 struct stat a,d;if(lstat(argv[2],&a)||lstat(argv[3],&d)||!S_ISDIR(a.st_mode)||!S_ISDIR(d.st_mode)||a.st_uid!=getuid()||d.st_uid!=getuid()||(a.st_dev==d.st_dev&&a.st_ino==d.st_ino))return 65;
 int abi=(int)syscall(__NR_landlock_create_ruleset,NULL,0,LANDLOCK_CREATE_RULESET_VERSION);
 if(abi<3){printf("{\"mode\":\"%s\",\"abi\":%d,\"policy_ready\":false,\"passed\":false,\"unsupported\":true}\n",modes[mode],abi);return 77;}
 char input[PATH_MAX],output[PATH_MAX],denied[PATH_MAX];
 if(snprintf(input,sizeof(input),"%s/input",argv[2])>=(int)sizeof(input)||snprintf(output,sizeof(output),"%s/output",argv[2])>=(int)sizeof(output)||snprintf(denied,sizeof(denied),"%s/canary",argv[3])>=(int)sizeof(denied))return 65;
 /* The unrestricted parent proves normal filesystem access and rejects
  * ambient sockets/terminal/service descriptors before creating child policy. */
 struct stat nullstat,standard;
 if(stat("/dev/null",&nullstat)||fstat(0,&standard)||!S_ISCHR(standard.st_mode)||standard.st_rdev!=nullstat.st_rdev)return 65;
 for(int i=1;i<=2;++i)if(fstat(i,&standard)||!S_ISREG(standard.st_mode)||standard.st_uid!=getuid())return 65;
 if(!canary_accessible(input)||!canary_accessible(denied))return 65;
 int pipefds[2];if(pipe2(pipefds,O_CLOEXEC|O_NONBLOCK))return 66;
 int inherited=-1;
 if(mode==12){int pair[2];if(socketpair(AF_UNIX,SOCK_STREAM|SOCK_CLOEXEC,0,pair))return 66;inherited=fcntl(pair[0],F_DUPFD_CLOEXEC,10);close(pair[0]);close(pair[1]);if(inherited!=10)return 66;}
 pid_t pid=fork();if(pid<0)return 66;
 if(pid==0)child(mode,pipefds[1],argv[2],input,output,denied);
 close(pipefds[1]);if(inherited>=0)close(inherited);
 int status=0,done=0,timedout=0;unsigned ready_count=0,bad_marker=0;double deadline=now()+5;
 while(!done){
  unsigned char buf[16];ssize_t n=read(pipefds[0],buf,sizeof(buf));if(n>0)for(ssize_t i=0;i<n;++i){++ready_count;if(buf[i]!=(ready_count==1?0xa5:0xa6))bad_marker=1;}
  pid_t got=waitpid(pid,&status,WNOHANG);if(got==pid){done=1;break;}if(got<0){kill(pid,SIGKILL);waitpid(pid,&status,0);timedout=1;break;}
  if(now()>=deadline){kill(pid,SIGKILL);waitpid(pid,&status,0);timedout=1;break;}
  struct timespec delay={0,1000000};nanosleep(&delay,NULL);
 }
 unsigned char buf[16];ssize_t n;while((n=read(pipefds[0],buf,sizeof(buf)))>0)for(ssize_t i=0;i<n;++i){++ready_count;if(buf[i]!=(ready_count==1?0xa5:0xa6))bad_marker=1;}
 close(pipefds[0]);int code=WIFEXITED(status)?WEXITSTATUS(status):-1;int sig=WIFSIGNALED(status)?WTERMSIG(status):0;
 unsigned expected_ready=(mode==13||mode==15)?2:1;
 int policy_ready=ready_count==expected_ready&&!bad_marker;
 int pass=!timedout && (mode==14 ? (ready_count==0&&code==77&&sig==0) : (policy_ready && (socket_mode(mode)?sig==SIGSYS:(sig==0&&code==0))));
 printf("{\"mode\":\"%s\",\"abi\":%d,\"policy_ready\":%s,\"ready_count\":%u,\"exit\":%d,\"signal\":%d,\"timedout\":%s,\"passed\":%s}\n",modes[mode],abi,policy_ready?"true":"false",ready_count,code,sig,timedout?"true":"false",pass?"true":"false");
 return pass?0:1;
}
