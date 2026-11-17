#define _GNU_SOURCE
/* Fixed post-exec target. Parent owns outcome/status/canary verification. */
#include <errno.h>
#include <asm/prctl.h>
#include <linux/sched.h>
#include <signal.h>
#include <sys/resource.h>
#include <time.h>
#include <fcntl.h>
#include <stdint.h>
#include <string.h>
#include <sys/socket.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#include "config.h"
static void mark(unsigned char x){if(syscall(__NR_write,4,&x,1)!=1)_exit(90);}
static void finish(int ok){_exit(ok?0:91);}
static int denied(const char *path,int flags){errno=0;int f=syscall(__NR_openat,AT_FDCWD,path,flags,0600);if(f>=0){syscall(__NR_close,f);return 0;}return errno==EACCES;}
/* x86_64 vfork probe never returns to C in the unexpected child. */
static long probe_vfork(void){
#if defined(__x86_64__) && !defined(__ILP32__)
 long result;
 __asm__ volatile("syscall\n\ttest %%rax,%%rax\n\tjnz 1f\n\tmov $60,%%rax\n\tmov $98,%%rdi\n\tsyscall\n\tud2\n1:"
  : "=a"(result) : "a"((long)__NR_vfork) : "rcx","r11","rdi","memory","cc");
 return result;
#else
 return -1;
#endif
}
static int valid_input(void){
 int f=syscall(__NR_openat,AT_FDCWD,ALLOWED_DIR "/input",O_RDONLY|O_CLOEXEC|O_NOFOLLOW,0);
 struct stat st;if(f<0||syscall(__NR_fstat,f,&st)||!S_ISREG(st.st_mode))_exit(94);return f;
}
int main(int argc,char **argv){
 if(argc!=2)_exit(92);mark(0xa2);const char *m=argv[1];
 if(!strcmp(m,"positive")){
  int f=syscall(__NR_openat,AT_FDCWD,ALLOWED_DIR "/input",O_RDONLY|O_NOFOLLOW,0);if(f<0)_exit(93);
  char b[14];ssize_t n=syscall(__NR_read,f,b,sizeof(b));syscall(__NR_close,f);if(n!=13||memcmp(b,"probe-canary\n",13))_exit(93);
  mark(0xa3);f=syscall(__NR_openat,AT_FDCWD,ALLOWED_DIR "/output",O_WRONLY|O_CREAT|O_EXCL|O_NOFOLLOW,0600);if(f<0)_exit(93);
  int ok=syscall(__NR_write,f,"probe-output\n",13)==13;syscall(__NR_close,f);finish(ok);
 }
 if(!strcmp(m,"exec-denied")){
  int f=syscall(__NR_openat,AT_FDCWD,DENIED_ELF,O_RDONLY|O_CLOEXEC|O_NOFOLLOW,0);if(f<0)_exit(94);
  struct stat a,b;if(syscall(__NR_fstat,f,&a)||syscall(__NR_newfstatat,AT_FDCWD,DENIED_ELF,&b,AT_SYMLINK_NOFOLLOW)||!S_ISREG(a.st_mode)||a.st_dev!=b.st_dev||a.st_ino!=b.st_ino)_exit(94);
  if(f!=3){if(syscall(__NR_dup3,f,3,O_CLOEXEC)!=3)_exit(94);syscall(__NR_close,f);}
  char *av[]={DENIED_ELF,NULL},*env[]={NULL};mark(0xa3);errno=0;long r=syscall(__NR_execveat,3,"",av,env,AT_EMPTY_PATH);finish(r==-1&&errno==EACCES);
 }
 if(!strcmp(m,"truncate-denied")){
  int f=syscall(__NR_openat,AT_FDCWD,DENIED_FILE,O_WRONLY|O_NOFOLLOW,0);if(f<0)_exit(95);
  mark(0xa3);errno=0;long r=syscall(__NR_ftruncate,f,0);int ok=r==-1&&errno==EACCES;syscall(__NR_close,f);finish(ok);
 }
 if(!strcmp(m,"inherited-fds")){
  for(int f=100;f<=102;f++){mark((unsigned char)(0xb0+f-100));errno=0;if(!(syscall(__NR_fcntl,f,F_GETFD)==-1&&errno==EBADF))_exit(96);}finish(1);
 }
 /* Every restricted-scalar probe retains the full machine-word value. */
 const unsigned long high=1UL<<32;
 if(!strncmp(m,"exec-fd-",8)||!strncmp(m,"exec-flags-",11)){
  int f=syscall(__NR_openat,AT_FDCWD,DENIED_ELF,O_RDONLY|O_CLOEXEC|O_NOFOLLOW,0);
  struct stat st;if(f<0||syscall(__NR_fstat,f,&st)||!S_ISREG(st.st_mode))_exit(94);
  if(f!=3){if(syscall(__NR_dup3,f,3,O_CLOEXEC)!=3)_exit(94);syscall(__NR_close,f);}
  unsigned long fd=3,flags=AT_EMPTY_PATH;
  if(!strcmp(m,"exec-fd-low")){int other=syscall(__NR_openat,AT_FDCWD,DENIED_ELF,O_RDONLY|O_CLOEXEC|O_NOFOLLOW,0);if(other<0||other==3)_exit(94);fd=(unsigned long)other;}
  if(!strcmp(m,"exec-fd-high"))fd|=high;
  if(!strcmp(m,"exec-flags-low"))flags=0;
  if(!strcmp(m,"exec-flags-high"))flags|=high;
  char *av[]={DENIED_ELF,NULL},*ev[]={NULL};mark(0xa3);syscall(__NR_execveat,fd,"",av,ev,flags);_exit(97);
 }
 if(!strncmp(m,"dup-dst-",8)||!strncmp(m,"dup-flags-",10)){
  int f=valid_input();unsigned long dst=3,flags=O_CLOEXEC;
  if(!strcmp(m,"dup-dst-low"))dst=5;
  if(!strcmp(m,"dup-dst-high"))dst|=high;
  if(!strcmp(m,"dup-flags-low"))flags=0;
  if(!strcmp(m,"dup-flags-high"))flags|=high;
  mark(0xa3);syscall(__NR_dup3,(unsigned long)f,dst,flags);_exit(97);
 }
 if(!strncmp(m,"fcntl-cmd-",10)){
  int f=valid_input();unsigned long cmd=!strcmp(m,"fcntl-cmd-low")?F_GETFL:(high|F_GETFD);
  mark(0xa3);syscall(__NR_fcntl,(unsigned long)f,cmd,0UL);_exit(97);
 }
 if(!strncmp(m,"arch-cmd-",9)){
  unsigned long value=0,cmd=!strcmp(m,"arch-cmd-low")?ARCH_GET_FS:(high|ARCH_SET_FS);
  mark(0xa3);syscall(__NR_arch_prctl,cmd,&value);_exit(97);
 }
 if(!strncmp(m,"prlimit-",8)){
  struct rlimit old={0},new={0};unsigned long pid=0,ptr=0;
  if(!strcmp(m,"prlimit-pid-low"))pid=1; /* Nonself query need not succeed; filter must SIGSYS before lookup. */
  if(!strcmp(m,"prlimit-pid-high"))pid=high;
  if(!strcmp(m,"prlimit-new-low"))ptr=(unsigned long)&new;
  if(!strcmp(m,"prlimit-new-high"))ptr=high;
  mark(0xa3);syscall(__NR_prlimit64,pid,(unsigned long)RLIMIT_CORE,ptr,&old);_exit(97);
 }
 if(!strcmp(m,"symlink-truncate")){
  int f=syscall(__NR_openat,AT_FDCWD,ALLOWED_DIR "/outside-link",O_WRONLY,0);if(f<0)_exit(95);
  mark(0xa3);errno=0;long r=syscall(__NR_ftruncate,f,0);int ok=r==-1&&errno==EACCES;syscall(__NR_close,f);finish(ok);
 }
 if(!strcmp(m,"fchmod-denied")||!strcmp(m,"fchown-denied")){
  int f=valid_input();mark(0xa3);
  if(!strcmp(m,"fchmod-denied"))syscall(__NR_fchmod,f,0777);else syscall(__NR_fchown,f,0,0);
  _exit(97);
 }
 mark(0xa3);
 if(!strcmp(m,"x32-denied"))syscall((long)__NR_getpid|0x40000000UL);
 else if(!strcmp(m,"execve-denied")){char *av[]={DENIED_ELF,NULL},*ev[]={NULL};syscall(__NR_execve,DENIED_ELF,av,ev);}
 else if(!strcmp(m,"fork-denied")){long r=syscall(__NR_fork);if(r==0)syscall(__NR_exit,98);}
 else if(!strcmp(m,"vfork-denied"))probe_vfork();
 else if(!strcmp(m,"clone-denied")){long r=syscall(__NR_clone,(unsigned long)SIGCHLD,0UL,0UL,0UL,0UL);if(r==0)syscall(__NR_exit,98);}
 else if(!strcmp(m,"clone3-denied")){struct clone_args a={.exit_signal=SIGCHLD};long r=syscall(__NR_clone3,&a,sizeof(a));if(r==0)syscall(__NR_exit,98);}
 else if(!strcmp(m,"chmod-denied"))syscall(__NR_chmod,DENIED_FILE,0777);
 else if(!strcmp(m,"fchmodat-denied"))syscall(__NR_fchmodat,AT_FDCWD,DENIED_FILE,0777);
 else if(!strcmp(m,"chown-denied"))syscall(__NR_chown,DENIED_FILE,0,0);
 else if(!strcmp(m,"utimensat-denied")){struct timespec t[2]={{.tv_sec=1},{.tv_sec=1}};syscall(__NR_utimensat,AT_FDCWD,DENIED_FILE,t,0);}
 else if(!strcmp(m,"unlinkat-denied"))syscall(__NR_unlinkat,AT_FDCWD,DENIED_FILE,0);
 else if(!strcmp(m,"renameat2-denied"))syscall(__NR_renameat2,AT_FDCWD,DENIED_FILE,AT_FDCWD,ALLOWED_DIR "/moved",0);
 else if(!strcmp(m,"linkat-denied"))syscall(__NR_linkat,AT_FDCWD,DENIED_FILE,AT_FDCWD,ALLOWED_DIR "/linked",0);
 else if(!strcmp(m,"symlink-write"))finish(denied(ALLOWED_DIR "/outside-link",O_WRONLY));
 else goto original_cases;
 _exit(97);
original_cases:
 if(!strcmp(m,"read-denied"))finish(denied(DENIED_FILE,O_RDONLY|O_NOFOLLOW));
 if(!strcmp(m,"write-denied"))finish(denied(DENIED_FILE,O_WRONLY|O_NOFOLLOW));
 if(!strcmp(m,"symlink-read"))finish(denied(ALLOWED_DIR "/outside-link",O_RDONLY));
 if(!strcmp(m,"socket-inet"))syscall(__NR_socket,AF_INET,SOCK_STREAM,0);
 else if(!strcmp(m,"socket-inet6"))syscall(__NR_socket,AF_INET6,SOCK_DGRAM,0);
 else if(!strcmp(m,"socket-unix"))syscall(__NR_socket,AF_UNIX,SOCK_STREAM,0);
 else if(!strcmp(m,"socketpair")){int pair[2];syscall(__NR_socketpair,AF_UNIX,SOCK_STREAM,0,pair);}
 else if(!strcmp(m,"io-uring"))syscall(__NR_io_uring_setup,1,NULL);
 else if(!strcmp(m,"ptrace"))syscall(__NR_ptrace,0,0,0,0);
 else if(!strcmp(m,"pidfd-getfd"))syscall(__NR_pidfd_getfd,-1,0,0);
 _exit(97);
}
