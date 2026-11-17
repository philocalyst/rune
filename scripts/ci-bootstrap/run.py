#!/usr/bin/env python3
"""Normal CLI/bootstrap diagnostic, never a substitute for an actual Doom profile."""
import hashlib,importlib.util,json,os,pathlib,re,resource,shutil,sys
sys.dont_write_bytecode=True
S=pathlib.Path.cwd().resolve(); P=S/'scripts/ci-bootstrap'
spec=importlib.util.spec_from_file_location('native_guard',S/'scripts/ci-v11/run.py');g=importlib.util.module_from_spec(spec);spec.loader.exec_module(g)
def sandbox(command,target=None):
 args=[str(U),'--user','--map-root-user','--net',str(B),'--die-with-parent','--unshare-pid','--unshare-ipc','--unshare-uts']
 for source in ['/usr','/lib','/lib64','/etc']:
  assert pathlib.Path(source).is_dir();args+=['--ro-bind',source,source]
 args+=['--symlink','usr/bin','/bin','--symlink','usr/sbin','/sbin','--proc','/proc','--dev','/dev','--tmpfs','/tmp','--tmpfs','/home','--dir','/home/rune-ci','--dir','/home/rune-ci/.cache','--dir','/home/rune-ci/.config','--dir','/home/rune-ci/.local','--dir','/home/rune-ci/.local/state','--dir','/run','--dir','/var','--symlink','/run','/var/run','--ro-bind',str(S),str(S)]
 if target is not None:args+=['--ro-bind',str(target),str(target)]
 args+=['--chdir',str(S),'--clearenv','--setenv','HOME','/home/rune-ci','--setenv','XDG_CACHE_HOME','/home/rune-ci/.cache','--setenv','XDG_CONFIG_HOME','/home/rune-ci/.config','--setenv','XDG_STATE_HOME','/home/rune-ci/.local/state','--setenv','TMPDIR','/tmp','--setenv','PATH','/usr/bin:/bin','--setenv','LANG','C.UTF-8','--setenv','LC_ALL','C.UTF-8','--setenv','TZ','UTC']
 return args+['--',*command]
def main():
 global U,B
 assert os.environ.get('GITHUB_ACTIONS')=='true' and os.environ.get('GITHUB_REPOSITORY')=='philocalyst/rune'
 assert os.environ.get('GITHUB_EVENT_NAME')=='push' and os.environ.get('GITHUB_REF')=='refs/heads/work/rune-backtrace-untested-20261117'
 g.COMMIT=os.environ['EXPECTED_COMMIT'];assert re.fullmatch('[0-9a-f]{40}',g.COMMIT) and g.COMMIT==os.environ['GITHUB_SHA'];g.verify()
 g.E=pathlib.Path(os.environ['RUNNER_TEMP']).resolve()/('rune-bootstrap-'+os.environ['GITHUB_RUN_ID']+'-'+os.environ['GITHUB_RUN_ATTEMPT']);g.E.mkdir(exist_ok=False)
 home=g.E/'home';home.mkdir();target=g.E/'target'
 u=shutil.which('unshare');assert u,'BLOCKED: unshare absent; no replacement or fallback'
 U=pathlib.Path(u).resolve();rustup=pathlib.Path(shutil.which('rustup')).resolve()
 g.ENV={'HOME':str(home),'CARGO_HOME':str(g.E/'cargo'),'RUSTUP_HOME':str(g.E/'rustup'),'PATH':str(rustup.parent)+':/usr/bin:/bin','LANG':'C.UTF-8','LC_ALL':'C.UTF-8','TZ':'UTC','CARGO_TARGET_DIR':str(target),'CARGO_BUILD_JOBS':'2','CARGO_INCREMENTAL':'0','GIT_TERMINAL_PROMPT':'0','RUSTUP_DIST_SERVER':'https://static.rust-lang.org','RUSTUP_UPDATE_ROOT':'https://static.rust-lang.org/rustup'}
 # Fetch the exact official Ubuntu package, but NEVER install it: its sysctl
 # configuration and maintainer scripts must not affect this hosted runner.
 assert shutil.disk_usage(g.E).free>=8*1024**3
 package_dir=g.E/'bubblewrap-package';package_dir.mkdir()
 package=package_dir/'bubblewrap_0.9.0-1ubuntu0.3_amd64.deb'
 package_pin='2461f1beee9cb04c8942739fe1a2b37e7b7c2a3d518f0779dc75f9245baa3094'
 apt=pathlib.Path('/usr/bin/apt-get');dpkg=pathlib.Path('/usr/bin/dpkg-deb')
 assert apt.is_file() and dpkg.is_file()
 g.run('bubblewrap-download',['/usr/bin/env','--chdir='+str(package_dir),str(apt),'-o','APT::Get::AllowUnauthenticated=false','-o','Acquire::AllowInsecureRepositories=false','-o','Acquire::Retries=0','download','bubblewrap:amd64=0.9.0-1ubuntu0.3'],60,1024**2,2*1024**2)
 assert package.is_file() and package.stat().st_size==50436 and g.sha(package)==package_pin
 extracted=package_dir/'extracted';extracted.mkdir()
 g.run('bubblewrap-extract',[str(dpkg),'--extract',str(package),str(extracted)],10,1024**2,2*1024**2)
 # Extracting a deb does not execute its scripts or apply files under /etc or
 # /usr/lib/sysctl.d. Only copy its ordinary executable to a fresh owned path.
 packaged_binary=extracted/'usr/bin/bwrap'
 assert packaged_binary.is_file() and not packaged_binary.is_symlink()
 assert packaged_binary.open('rb').read(4)==b'\x7fELF'
 B=package_dir/'bwrap';shutil.copyfile(packaged_binary,B);B.chmod(0o700)
 g.save(g.E/'bubblewrap-package.json',{'version':'0.9.0-1ubuntu0.3','architecture':'amd64','package_sha256':package_pin,'binary_sha256':g.sha(B),'apt_sha256':g.sha(apt),'dpkg_deb_sha256':g.sha(dpkg),'installed':False,'security_settings_changed':False,'source':'https://packages.ubuntu.com/noble-updates/amd64/bubblewrap/download'})
 pins={str(p):g.sha(p) for p in [U,B,package,P/'check-sandbox.py',P/'checkpoint.el',P/'run.py',S/'scripts/ci-v11/run.py']}
 def verify():
  g.verify();assert all(g.sha(pathlib.Path(n))==h for n,h in pins.items())
 net=os.readlink('/proc/self/ns/net');pid=os.readlink('/proc/self/ns/pid')
 inspector=['/usr/bin/python3',str(P/'check-sandbox.py'),str(S),net,pid]
 assert shutil.disk_usage(g.E).free>=8*1024**3
 # Must succeed before any dependency fetch or compilation. Never relax kernel settings.
 admission,_=g.run('sandbox-admission',sandbox(inspector),10,1024**2,1024**2)
 assert json.loads(admission)['sandbox_admission'] is True;verify()
 uv,_=g.run('unshare-version',[str(U),'--version'],5,65536,65536);bv,_=g.run('bwrap-version',[str(B),'--version'],5,65536,65536);assert bv.strip()=='bubblewrap 0.9.0'
 g.run('toolchain',[str(rustup),'toolchain','install','1.96.0','--profile','minimal','--no-self-update'],180)
 tool=g.E/'rustup/toolchains/1.96.0-x86_64-unknown-linux-gnu/bin';cargo=tool/'cargo';rustc=tool/'rustc';assert cargo.is_file() and rustc.is_file();g.ENV['PATH']=str(tool)+':/usr/bin:/bin'
 rv,_=g.run('rust-version',[str(rustc),'-Vv'],10);cv,_=g.run('cargo-version',[str(cargo),'-Vv'],10);assert rv.startswith('rustc 1.96.0 ') and cv.startswith('cargo 1.96.0 ')
 g.run('fetch',[str(cargo),'fetch','--locked','--target','x86_64-unknown-linux-gnu'],180);verify()
 output,_=g.run('build-cli',[str(cargo),'build','--locked','--offline','--no-default-features','--bin','rune','-j2','--message-format=json-render-diagnostics'],600)
 artifacts=[]
 for line in output.splitlines():
  try:item=json.loads(line)
  except json.JSONDecodeError:continue
  if item.get('reason')=='compiler-artifact' and item.get('target',{}).get('name')=='rune' and item.get('target',{}).get('kind')==['bin'] and item.get('profile',{}).get('test') is False and item.get('executable'):
   assert pathlib.Path(item['target']['src_path']).resolve()==S/'src/main.rs';artifacts.append(item)
 assert len(artifacts)==1;artifact=artifacts[0];binary=pathlib.Path(artifact['executable']).resolve();assert binary.is_relative_to(target.resolve()) and binary.is_file();assert binary.open('rb').read(4)==b'\x7fELF'
 bp=g.sha(binary);verify()
 g.save(g.E/'cli-provenance.json',{'commit':g.COMMIT,'source_pin':g.SOURCE_PIN,'cargo_artifact':artifact,'binary_sha256':bp,'rustc':rv,'cargo':cv,'rustc_sha256':g.sha(rustc),'cargo_sha256':g.sha(cargo),'sandbox_tools':pins,'unshare':uv,'bwrap':bv,'image':os.environ.get('ImageVersion'),'scope':'bundled normal bootstrap checkpoint only; no Doom profile'})
 def cli_caps(filecap):
  resource.setrlimit(resource.RLIMIT_CORE,(0,0));resource.setrlimit(resource.RLIMIT_AS,(4*1024**3,4*1024**3));resource.setrlimit(resource.RLIMIT_CPU,(90,90));resource.setrlimit(resource.RLIMIT_FSIZE,(filecap,filecap))
 g.caps=cli_caps
 # Inspector verifies the actual workload namespace then execs the NORMAL CLI.
 cmd=sandbox(inspector+['--target',str(target),'--exec',str(binary),'--load',str(P/'checkpoint.el')],target)
 stdout,stderr=g.run('normal-bootstrap',cmd,120,4*1024**2,2*1024**2)
 marker='RUNE-NORMAL-BOOTSTRAP-REACHED-2FCA7C0';assert stdout.splitlines().count(marker)==1
 assert g.sha(binary)==bp;verify()
 result={'normal_bootstrap_checkpoint_reached':True,'commit':g.COMMIT,'binary_sha256':bp,'source_pin':g.SOURCE_PIN,'doom_profile_present':False,'doom_startup_tested':False,'gnu_reference_suite_tested':False,'old_local_process':'UNKNOWN/unaffected'}
 g.save(g.E/'result.json',result);print(json.dumps(result),flush=True)
 if os.environ.get('GITHUB_STEP_SUMMARY'):pathlib.Path(os.environ['GITHUB_STEP_SUMMARY']).write_text('Normal bundled bootstrap checkpoint reached at '+g.COMMIT+'. No Doom profile or GNU139 qualification.\n')
if __name__=='__main__':main()
