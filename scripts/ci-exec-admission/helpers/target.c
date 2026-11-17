#define _GNU_SOURCE
/* Fixed post-exec target. Parent owns outcome/status/canary verification. */
#include <errno.h>
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
 mark(0xa3);
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
