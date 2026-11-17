#define _GNU_SOURCE
/* Fixed SYNTHETIC exec mechanism only, not GNU launcher or runtime policy. */
#include <errno.h>
#include <asm/prctl.h>
#include <fcntl.h>
#include <linux/audit.h>
#include <linux/filter.h>
#include <linux/landlock.h>
#include <linux/seccomp.h>
#include <stddef.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>
#include <sys/prctl.h>
#include <sys/stat.h>
#include <sys/syscall.h>
#include <unistd.h>
#include "config.h"
#ifndef LANDLOCK_ACCESS_FS_TRUNCATE
#define LANDLOCK_ACCESS_FS_TRUNCATE (1ULL<<14)
#endif
static const char *modes[]={"positive","read-denied","write-denied","truncate-denied","exec-denied","socket-inet","socket-inet6","socket-unix","socketpair","io-uring","ptrace","pidfd-getfd","inherited-fds","symlink-read","invalid-policy"};
#define ALLOW(n) BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,(n),0,1),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW)
#define ARGLO(n) (offsetof(struct seccomp_data,args)+(n)*8)
#define ARGHI(n) (ARGLO(n)+4)
#define EQARG(n,value) BPF_STMT(BPF_LD|BPF_W|BPF_ABS,ARGHI(n)),BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,0,1,0),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS),BPF_STMT(BPF_LD|BPF_W|BPF_ABS,ARGLO(n)),BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,(value),1,0),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS)
static int filter(void){
#if !defined(__x86_64__) || defined(__ILP32__)
 return -1;
#else
 struct sock_filter code[]={
 BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,arch)),BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,AUDIT_ARCH_X86_64,1,0),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS),
 BPF_STMT(BPF_LD|BPF_W|BPF_ABS,offsetof(struct seccomp_data,nr)),BPF_JUMP(BPF_JMP|BPF_JSET|BPF_K,0x40000000,0,1),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS),
 /* Exact 64-bit scalar args. A mismatch kills; no low-word truncation. */
 BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,__NR_execveat,0,13),EQARG(0,3),EQARG(4,AT_EMPTY_PATH),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW),
 BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,__NR_dup3,0,13),EQARG(1,3),EQARG(2,O_CLOEXEC),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW),
 BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,__NR_fcntl,0,7),EQARG(1,F_GETFD),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW),
 BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,__NR_arch_prctl,0,7),EQARG(0,ARCH_SET_FS),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW),
 BPF_JUMP(BPF_JMP|BPF_JEQ|BPF_K,__NR_prlimit64,0,13),EQARG(0,0),EQARG(2,0),BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_ALLOW),
 /* Fixed dynamic-loader and tiny target candidate, not GNU completeness. */
 ALLOW(__NR_read),ALLOW(__NR_write),ALLOW(__NR_close),ALLOW(__NR_openat),ALLOW(__NR_newfstatat),ALLOW(__NR_fstat),ALLOW(__NR_lseek),ALLOW(__NR_pread64),
 ALLOW(__NR_mmap),ALLOW(__NR_mprotect),ALLOW(__NR_munmap),ALLOW(__NR_brk),
 ALLOW(__NR_set_tid_address),ALLOW(__NR_set_robust_list),ALLOW(__NR_rseq),ALLOW(__NR_getrandom),
 ALLOW(__NR_rt_sigaction),ALLOW(__NR_rt_sigprocmask),ALLOW(__NR_rt_sigreturn),ALLOW(__NR_futex),ALLOW(__NR_access),ALLOW(__NR_readlink),
 ALLOW(__NR_ftruncate),ALLOW(__NR_exit),ALLOW(__NR_exit_group),
 BPF_STMT(BPF_RET|BPF_K,SECCOMP_RET_KILL_PROCESS)};
 struct sock_fprog p={.len=sizeof(code)/sizeof(code[0]),.filter=code};return syscall(__NR_seccomp,SECCOMP_SET_MODE_FILTER,0,&p);
#endif
}
static int add(int rules,const char *name,uint64_t rights,int dir){
 int fd=open(name,O_PATH|O_CLOEXEC|O_NOFOLLOW|(dir?O_DIRECTORY:0));if(fd<0)return -1;
 struct stat st;if(fstat(fd,&st)||(!dir&&!S_ISREG(st.st_mode))||(dir&&!S_ISDIR(st.st_mode))){close(fd);return -1;}
 struct landlock_path_beneath_attr p={.allowed_access=rights,.parent_fd=fd};int r=syscall(__NR_landlock_add_rule,rules,LANDLOCK_RULE_PATH_BENEATH,&p,0);close(fd);return r;
}
static int policy(int truncate){
 struct landlock_ruleset_attr a={.handled_access_fs=(1ULL<<15)-1};int r=syscall(__NR_landlock_create_ruleset,&a,sizeof(a),0);if(r<0)return -1;
 uint64_t ro=LANDLOCK_ACCESS_FS_READ_FILE,rx=ro|LANDLOCK_ACCESS_FS_EXECUTE;
 int bad=add(r,TARGET_ELF,rx,0)||add(r,TARGET_INTERPRETER,rx,0)||add(r,DENIED_ELF,ro,0);
 for(size_t i=0;!bad&&i<sizeof(runtime_read_files)/sizeof(runtime_read_files[0]);i++)bad=add(r,runtime_read_files[i],ro,0);
 if(!bad)bad=add(r,ALLOWED_DIR,ro|LANDLOCK_ACCESS_FS_READ_DIR|LANDLOCK_ACCESS_FS_WRITE_FILE|LANDLOCK_ACCESS_FS_MAKE_REG|LANDLOCK_ACCESS_FS_REMOVE_FILE|LANDLOCK_ACCESS_FS_TRUNCATE,1);
 if(!bad&&truncate)bad=add(r,DENIED_FILE,LANDLOCK_ACCESS_FS_WRITE_FILE,0);
 if(!bad)bad=syscall(__NR_landlock_restrict_self,r,0);close(r);return bad?-1:0;
}
static int output_fd(int fd){
 struct stat s;if(fstat(fd,&s))return 0;int mode=fcntl(fd,F_GETFL);if(mode<0||(mode&O_ACCMODE)==O_RDONLY)return 0;
 if(S_ISREG(s.st_mode))return s.st_uid==getuid();
 if(!S_ISFIFO(s.st_mode))return 0;
 char path[64],link[128];snprintf(path,sizeof(path),"/proc/self/fd/%d",fd);ssize_t n=readlink(path,link,sizeof(link)-1);if(n<0)return 0;link[n]=0;
 return n>7&&!strncmp(link,"pipe:[",6)&&link[n-1]==']';
}
int main(int argc,char **argv){
 if(argc!=2)return 64;int m=-1;for(size_t i=0;i<sizeof(modes)/sizeof(modes[0]);i++)if(!strcmp(argv[1],modes[i]))m=i;if(m<0)return 64;
 struct stat s,z;if(stat("/dev/null",&z)||fstat(0,&s)||!S_ISCHR(s.st_mode)||s.st_rdev!=z.st_rdev||!output_fd(1)||!output_fd(2)||!output_fd(4))return 65;
 /* FD4 is admission-only trusted parent marker pipe, never child JSON. */
 if(fstat(4,&s)||!S_ISFIFO(s.st_mode))return 65;
 int fd=open(TARGET_ELF,O_RDONLY|O_CLOEXEC|O_NOFOLLOW);if(fd<0)return 66;
 if(fstat(fd,&s)||!S_ISREG(s.st_mode)||s.st_nlink!=1){close(fd);return 66;}
 struct stat denied;if(lstat(DENIED_ELF,&denied)||!S_ISREG(denied.st_mode)||denied.st_nlink!=1||s.st_dev!=denied.st_dev||s.st_ino==denied.st_ino||!(s.st_mode&0111)||!(denied.st_mode&0111)||access(TARGET_ELF,X_OK)||access(DENIED_ELF,X_OK))return 66;
 if(fd!=3){if(dup3(fd,3,O_CLOEXEC)!=3)return 66;close(fd);}else if(fcntl(3,F_SETFD,FD_CLOEXEC))return 66;
 if(syscall(__NR_close_range,5U,~0U,0))return 67;
 if(fcntl(4,F_SETFD,0))return 67;
 if(prctl(PR_SET_NO_NEW_PRIVS,1,0,0,0)||prctl(PR_GET_NO_NEW_PRIVS,0,0,0,0)!=1)return 68;
 if(syscall(__NR_landlock_create_ruleset,NULL,0,LANDLOCK_CREATE_RULESET_VERSION)<3)return 69;
 if(m==14){struct landlock_ruleset_attr invalid={.handled_access_fs=1ULL<<63};errno=0;int r=syscall(__NR_landlock_create_ruleset,&invalid,sizeof(invalid),0);if(r>=0){close(r);return 70;}return errno==EINVAL?77:70;}
 if(policy(m==3)||filter())return 71;
 char *target_argv[]={TARGET_ELF,argv[1],NULL};char *env[]={"LANG=C","LC_ALL=C","TZ=UTC",NULL};
 unsigned char mark=0xa1;if(syscall(__NR_write,4,&mark,1)!=1)_exit(72);
 syscall(__NR_execveat,3,"",target_argv,env,AT_EMPTY_PATH);_exit(73);
}
