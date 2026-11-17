#!/usr/bin/env python3
"""Prepare one fixed synthetic configuration as data; no build/ELF execution."""
import hashlib,json,os,pathlib,stat,sys
HERE=pathlib.Path(__file__).resolve().parent
def sha(p):return hashlib.sha256(p.read_bytes()).hexdigest()
def main():
 assert len(sys.argv)==2
 root=pathlib.Path(sys.argv[1]);assert root.is_absolute() and root.resolve()==root and root.is_dir() and not list(root.iterdir())
 st=root.stat();assert st.st_uid==os.getuid() and stat.S_IMODE(st.st_mode)==0o700
 # Fixed matched-host paths only; explicit generated table is still reviewed.
 runtime=[]
 for name in ['/lib64/ld-linux-x86-64.so.2','/lib/x86_64-linux-gnu/libc.so.6']:
  p=pathlib.Path(name).resolve();s=p.stat();assert p.is_relative_to('/usr/lib') and stat.S_ISREG(s.st_mode) and s.st_uid==0 and not s.st_mode&0o022
  assert p.open('rb').read(4)==b'\x7fELF';runtime.append({'path':str(p),'sha256':sha(p),'device':s.st_dev,'inode':s.st_ino})
 for d in ['allowed','outside','home','tmp']:(root/d).mkdir(mode=0o700)
 (root/'allowed/input').write_bytes(b'probe-canary\n');(root/'outside/canary').write_bytes(b'probe-canary\n')
 (root/'allowed/input').chmod(0o600);(root/'outside/canary').chmod(0o600)
 (root/'allowed/outside-link').symlink_to('../outside/canary')
 constants={'TARGET_ELF':str(root/'target'),'DENIED_ELF':str(root/'denied-target'),'ALLOWED_DIR':str(root/'allowed'),'DENIED_FILE':str(root/'outside/canary'),'TARGET_INTERPRETER':runtime[0]['path']}
 config='/* Fixed synthetic only. Generated data; independently review/pin. */\n'+''.join('#define '+k+' '+json.dumps(v)+'\n' for k,v in constants.items())
 config+='static const char *const runtime_read_files[]={'+','.join(json.dumps(v['path']) for v in runtime)+'};\n'
 (root/'config.h').write_text(config);(root/'config.h').chmod(0o400)
 receipt={'root':str(root),'runtime':runtime,'constants':constants,'config_sha256':sha(root/'config.h'),'source_hashes':{n:sha(HERE/n) for n in ['launcher.c','target.c']},'compiled':False,'gnu_executed':False}
 with (root/'config.json').open('x') as f:json.dump(receipt,f,indent=2);f.write('\n')
if __name__=='__main__':main()
