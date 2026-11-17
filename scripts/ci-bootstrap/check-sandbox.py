import json,os,pathlib,sys
source=pathlib.Path(sys.argv[1]); host_net=sys.argv[2];host_pid=sys.argv[3]
assert os.readlink('/proc/self/ns/net')!=host_net
assert os.readlink('/proc/self/ns/pid')!=host_pid
assert os.statvfs(source).f_flag & os.ST_RDONLY
routes=pathlib.Path('/proc/net/route').read_text().splitlines()[1:]
assert not any(line.strip() for line in routes),'unexpected IPv4 routes'
ipv6=pathlib.Path('/proc/net/ipv6_route')
if ipv6.exists():
 assert not any(line.split() and line.split()[-1]!='lo' for line in ipv6.read_text().splitlines()),'nonloopback IPv6 route'
assert pathlib.Path('/var/run').resolve()==pathlib.Path('/run')
assert not list(pathlib.Path('/run').iterdir())
# /home is fresh tmpfs, but original checkout/target mounts require ancestors.
# Walk every ancestor directory; only explicitly mounted roots may hide children.
roots={source,pathlib.Path('/home/rune-ci')}
if len(sys.argv)>4:
 assert len(sys.argv)==10 and sys.argv[4]=='--target' and sys.argv[6]=='--exec'
 target=pathlib.Path(sys.argv[5]);assert target.is_absolute() and target.resolve()==target
 assert pathlib.Path(sys.argv[7]).is_relative_to(target)
 assert os.statvfs(target).f_flag & os.ST_RDONLY
 roots.add(target)
assert source.is_absolute() and source.resolve()==source
allowed=set()
for root in roots:
 if root.is_relative_to('/home'):
  allowed.add(root);allowed.update(p for p in root.parents if p.is_relative_to('/home'))
def check_home(directory):
 for child in directory.iterdir():
  assert child in allowed and not child.is_symlink(),'unexpected home entry: '+str(child)
  if child not in roots:
   assert child.is_dir();check_home(child)
check_home(pathlib.Path('/home'))
home=pathlib.Path(os.environ['HOME']); probe=home/'admission-write';probe.write_text('owned');assert probe.read_text()=='owned';probe.unlink()
print(json.dumps({'sandbox_admission':True,'source_readonly':True,'private_net_pid':True,'host_run_sockets_hidden':True,'home_writable':True}),flush=True)
if len(sys.argv)>4:
 os.execv(sys.argv[7],sys.argv[7:])
