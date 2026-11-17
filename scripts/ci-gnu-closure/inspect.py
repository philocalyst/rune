#!/usr/bin/env python3
"""Fixed-host ELF inspection only: never execute an ELF or dynamic loader."""
import hashlib,json,os,pathlib,re,stat,subprocess,sys,time
DIRS=('/lib/x86_64-linux-gnu','/usr/lib/x86_64-linux-gnu','/lib64','/usr/lib64','/lib','/usr/lib')
MAX_FILES=256;MAX_OUTPUT=32*1024**2
READ_ELF='/usr/bin/readelf'
EXPECTED_NEEDED=set('libasound.so.2 libacl.so.1 libattr.so.1 libdbus-1.so.3 libxml2.so.2 libgpm.so.2 libtinfo.so.6 libselinux.so.1 libgnutls.so.30 liblcms2.so.2 libm.so.6 libz.so.1 libsystemd.so.0 libgmp.so.10 libgccjit.so.0 libtree-sitter.so.0 libsqlite3.so.0 libc.so.6'.split())
def sha(p):
 h=hashlib.sha256()
 with p.open('rb') as f:
  while b:=f.read(1024**2):h.update(b)
 return h.hexdigest()
def need(b,msg):
 if not b:raise RuntimeError(msg)
def parse(text):
 need(re.search(r'Class:\s+ELF64\b',text) and re.search(r'Data:\s+2.s complement, little endian',text) and re.search(r'Machine:\s+Advanced Micro Devices X86-64',text),'unexpected ELF ABI')
 headers=re.findall(r'^Dynamic section .* contains (\d+) entries:',text,re.M);need(len(headers)==1,'one dynamic table required')
 rows=re.findall(r'^\s*0x[0-9a-fA-F]+\s+\(([^)]+)\)\s*(.*)$',text,re.M)
 need(len(rows)==int(headers[0]),'unconsumed dynamic table rows')
 known=set('NEEDED PLTRELSZ PLTGOT HASH STRTAB SYMTAB RELA RELASZ RELAENT STRSZ SYMENT INIT FINI SONAME RPATH SYMBOLIC REL RELSZ RELENT PLTREL DEBUG TEXTREL JMPREL BIND_NOW INIT_ARRAY FINI_ARRAY INIT_ARRAYSZ FINI_ARRAYSZ RUNPATH FLAGS PREINIT_ARRAY PREINIT_ARRAYSZ SYMTAB_SHNDX RELRSZ RELR RELRENT GNU_HASH VERSYM VERDEF VERDEFNUM VERNEED VERNEEDNUM RELACOUNT RELCOUNT FLAGS_1 NULL AUDIT DEPAUDIT FILTER AUXILIARY'.split())
 need(all(tag in known for tag,_ in rows),'unknown dynamic tag')
 dynamic={};values={}
 for tag,value in rows:
  values.setdefault(tag,[]).append(value)
  if tag in ('NEEDED','SONAME','RPATH','RUNPATH','AUDIT','DEPAUDIT','FILTER','AUXILIARY'):
   m=re.search(r'\[([^\]]*)\]',value);need(m is not None,'unparsed path-sensitive dynamic tag');dynamic.setdefault(tag,[]).append(m.group(1))
 need(not any(dynamic.get(t) for t in ('RPATH','RUNPATH','AUDIT','DEPAUDIT','FILTER','AUXILIARY')),'unsupported search/audit tag')
 for n in dynamic.get('NEEDED',[]):need(bool(re.fullmatch(r'[A-Za-z0-9_+.-]+',n)) and n not in ('.','..'),'unsafe needed name')
 interp=re.findall(r'\[Requesting program interpreter: ([^\]]+)\]',text);need(len(interp)<=1,'multiple interpreters')
 sections=re.findall(r"^Symbol table '\.dynsym' contains (\d+) entries:\n(.*?)(?=^\S|\Z)",text,re.M|re.S);need(len(sections)==1,'dynamic symbol table required')
 count=int(sections[0][0]);need(count>1,'empty dynamic symbol table');symbols=[];indices=[]
 for line in sections[0][1].splitlines():
  if not line.strip() or line.strip().startswith('Num:'):continue
  m=re.fullmatch(r'\s*(\d+):\s+([0-9a-fA-F]+)\s+(?:0x[0-9a-fA-F]+|\d+)\s+(\S+)\s+(LOCAL|GLOBAL|WEAK|UNIQUE)\s+(DEFAULT|PROTECTED|HIDDEN|INTERNAL)\s+(\S+)(?:\s+(.*?))?\s*',line)
  need(m is not None,'unparsed dynamic symbol row');index,value,kind,bind,vis,ndx,name=m.groups();indices.append(int(index));name=name or ''
  if not name:need(int(index)==0 and bind=='LOCAL','unexpected nameless symbol');continue
  n=re.fullmatch(r'(\S+)(?:\s+\(\d+\))?',name);need(n is not None,'unparsed symbol name/version');base,sep,ver=n.group(1).partition('@');default=ver.startswith('@');ver=ver.lstrip('@')
  if bind!='LOCAL' and vis in ('DEFAULT','PROTECTED'):symbols.append({'name':base,'version':ver or None,'default':default,'undefined':ndx=='UND','weak':bind=='WEAK','type':kind,'binding':bind})
 need(indices==list(range(count)),'dynamic symbol count/index mismatch')
 definitions=set();requirements={}
 defs=re.findall(r'^Version definition section .* contains (\d+) entries:\n(.*?)(?=^\S|\Z)',text,re.M|re.S)
 needs=re.findall(r'^Version needs section .* contains (\d+) entries:\n(.*?)(?=^\S|\Z)',text,re.M|re.S)
 need(len(defs)<=1 and len(needs)<=1,'duplicate version sections')
 for declared,body in defs:
  names=re.findall(r'\bName: (\S+)',body);need(len(names)==int(declared) and len(names)==len(set(names)),'unparsed definition count');definitions.update(names)
 for declared,body in needs:
  files=list(re.finditer(r'File: (\S+)\s+Cnt: (\d+)',body));need(len(files)==int(declared),'unparsed version provider count')
  for i,m in enumerate(files):
   provider,number=m.groups();part=body[m.end():files[i+1].start() if i+1<len(files) else len(body)];names=re.findall(r'\bName: (\S+)',part)
   need(provider not in requirements and len(names)==int(number) and len(names)==len(set(names)),'unparsed version requirement count');requirements[provider]=set(names)
 for tag,section in (('VERDEFNUM',defs),('VERNEEDNUM',needs)):
  if tag in values:need(len(values[tag])==1 and len(section)==1 and int(values[tag][0],0)==int(section[0][0]),'dynamic/version count mismatch')
  else:need(not section,'version table without dynamic count')
 vs=re.findall(r'^Version symbols section .* contains (\d+) entries:',text,re.M)
 if 'VERSYM' in values:need(len(vs)==1 and int(vs[0])==count,'version symbol count mismatch')
 else:need(not vs and not any(s['version'] for s in symbols),'missing version symbol table')
 return {'needed':dynamic.get('NEEDED',[]),'soname':dynamic.get('SONAME',[]),'interpreter':interp[0] if interp else None,'version_definitions':sorted(definitions),'version_requirements':{k:sorted(v) for k,v in requirements.items()},'symbols':symbols,'dynamic_entry_count':len(rows),'dynamic_symbol_count':count}
def main():
 need(len(sys.argv)==4,'PAYLOAD_ROOT FRESH_EVIDENCE_DIRECTORY PINNED_PAYLOAD_INVENTORY')
 root=pathlib.Path(sys.argv[1]);out=pathlib.Path(sys.argv[2]);need(root.is_absolute() and root.resolve()==root and out.is_absolute() and out.parent.resolve()==out.parent,'canonical paths')
 for p in (root,out.parent):need(p.is_dir() and p.stat().st_uid==os.getuid() and stat.S_IMODE(p.stat().st_mode)==0o700,'private root/parent')
 out.mkdir(mode=0o700);deadline=time.monotonic()+100
 elf=root/'usr/bin/emacs-nox';elns=sorted((root/'usr/lib/emacs/30.1/native-lisp').rglob('*.eln'));pdumps=sorted((root/'usr/libexec/emacs/30.1/x86_64-linux-gnu').glob('*.pdmp'))
 need(len(elns)==149 and len(pdumps)==1,'unexpected native/dump closure roots')
 tool=pathlib.Path(READ_ELF).resolve();toolpin=sha(tool);roots=[elf,*elns]
 manifest=pathlib.Path(sys.argv[3]);need(sha(manifest)=='09b5580652e007149f550fbb02feb0350dcc2274ebe05ff1605125230c16e7fd','payload inventory pin')
 expected={r['path']:r['sha256'] for rows in json.loads(manifest.read_text()).values() for r in rows if r['type']=='0'}
 for p in [*roots,*pdumps]:
  st=p.lstat();need(stat.S_ISREG(st.st_mode) and st.st_uid==os.getuid() and st.st_nlink==1 and stat.S_IMODE(st.st_mode)==0o400 and sha(p)==expected[str(p.relative_to(root))],'payload root identity')
 queue=list(roots);records={};edges={};missing=set();issues=[];providers={};resolved_aliases={}
 snapshots={}
 def snapshot(p):
  try:st=p.lstat()
  except FileNotFoundError:return None
  return (st.st_dev,st.st_ino,st.st_mode,st.st_uid,st.st_gid,st.st_size if not stat.S_ISDIR(st.st_mode) else None,os.readlink(p) if p.is_symlink() else None,str(p.resolve()))
 def watch(p):
  for item in [*reversed(p.parents),p]:
   key=str(item);value=snapshot(item)
   if key in snapshots:need(snapshots[key]==value,'path changed during resolution')
   else:snapshots[key]=value
 for p in [*roots,*pdumps,tool,pathlib.Path(READ_ELF)]:watch(p)
 allowed_dirs=[]
 for d in DIRS:
  p=pathlib.Path(d);watch(p)
  if p.is_dir() and p.resolve() not in allowed_dirs:allowed_dirs.append(p.resolve())
 # GCCjit may be installed in a versioned toolchain directory. Only fixed
 # numeric versions are inspected, never arbitrary /usr/local/user paths.
 gcc=pathlib.Path('/usr/lib/gcc/x86_64-linux-gnu')
 gcc_dirs=[]
 watch(gcc)
 for version in (12,13,14):
  p=gcc/str(version);watch(p)
  if p.is_dir():gcc_dirs.append(p.resolve())
 def resolve(name):
  candidates={};aliases=[]
  for d in allowed_dirs+(gcc_dirs if name=='libgccjit.so.0' else []):
   p=d/name;watch(p)
   if p.exists():
    q=p.resolve();st=q.stat();need(stat.S_ISREG(st.st_mode) and st.st_uid==0 and not st.st_mode&0o022,'untrusted library provider')
    need(q.is_relative_to('/usr/lib') or q.is_relative_to('/lib'),'library escaped system lib roots');candidates[str(q)]=q;aliases.append({'path':str(p),'canonical':str(q),'link':os.readlink(p) if p.is_symlink() else None})
  resolved_aliases[name]=aliases
  if not candidates:missing.add(name);return None
  if len(candidates)>1:issues.append({'ambiguous_provider':name,'paths':sorted(candidates)});return None
  return next(iter(candidates.values()))
 while queue:
  p=queue.pop(0).resolve()
  if str(p) in records:continue
  need(len(records)<MAX_FILES and time.monotonic()<deadline,'closure bound')
  st=p.lstat();need(stat.S_ISREG(st.st_mode) and st.st_size<=256*1024**2,'invalid ELF file');pin=sha(p)
  n=len(records);stdout=out/(str(n)+'.readelf');stderr=out/(str(n)+'.stderr')
  with stdout.open('xb') as o,stderr.open('xb') as e:
   result=subprocess.run([str(tool),'-W','-h','-l','-d','-V','--dyn-syms',str(p)],stdin=subprocess.DEVNULL,stdout=o,stderr=e,env={'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C','DEBUGINFOD_URLS':''},close_fds=True,timeout=min(5,max(.1,deadline-time.monotonic())))
  need(result.returncode==0 and stdout.stat().st_size<=MAX_OUTPUT and stderr.stat().st_size==0,'static parser failed/warning/bound')
  try:info=parse(stdout.read_text())
  except Exception as error:raise RuntimeError(str(p)+': '+str(error)) from error
  need(sha(p)==pin,'ELF changed during inspection');info.update({'path':str(p),'sha256':pin,'size':st.st_size,'device':st.st_dev,'inode':st.st_ino,'owner':st.st_uid,'mode':oct(stat.S_IMODE(st.st_mode)),'readelf_sha256':sha(stdout)})
  if p==elf.resolve():need(len(info['needed'])==18 and set(info['needed'])==EXPECTED_NEEDED and info['dynamic_symbol_count']==794 and len(info['version_requirements'])==12,'known GNU metadata coverage mismatch')
  records[str(p)]=info;edges[str(p)]={}
  for name in info['needed']:
   q=resolve(name)
   if q is not None:edges[str(p)][name]=str(q);queue.append(q);providers[name]=str(q)
  if info['interpreter']:
   need(info['interpreter']=='/lib64/ld-linux-x86-64.so.2','unexpected interpreter');watch(pathlib.Path(info['interpreter']));q=pathlib.Path(info['interpreter']).resolve();need((q.is_relative_to('/usr/lib') or q.is_relative_to('/lib')) and q.is_file() and q.stat().st_uid==0 and not q.stat().st_mode&0o022,'unsafe interpreter');queue.append(q)
 for path,r in records.items():
  for name,provider in edges[path].items():
   if records[provider]['soname']!=[name]:issues.append({'soname_mismatch':name,'provider':provider,'declared':records[provider]['soname']})
  for name,versions in r['version_requirements'].items():
   provider=edges[path].get(name)
   if provider is None:issues.append({'unresolved_version_provider':name,'consumer':path});continue
   absent=set(versions)-set(records[provider]['version_definitions'])
   if absent:issues.append({'missing_versions':sorted(absent),'provider':provider,'consumer':path})
 # Static global symbol compatibility is diagnostic only: actual namespace
 # ordering, IFUNC, dlopen, hwcaps and preloaded module behavior are unproven.
 exports={};default_exports=set()
 for path,r in records.items():
  for s in r['symbols']:
   if not s['undefined']:
    exports.setdefault((s['name'],s['version']),[]).append(path)
    if s['default'] or s['version'] is None:default_exports.add(s['name'])
 for path,r in records.items():
  for s in r['symbols']:
   if not s['undefined'] or s['weak']:continue
   if s['version']:
    ok=(s['name'],s['version']) in exports
   else:
    ok=s['name'] in default_exports
   if not ok:issues.append({'unresolved_strong_symbol':s['name'],'version':s['version'],'consumer':path})
 dumps=[{'path':str(p),'size':p.stat().st_size,'sha256':sha(p)} for p in pdumps]
 for path,r in records.items():
  p=pathlib.Path(path);st=p.stat()
  need(sha(p)==r['sha256'] and (st.st_dev,st.st_ino,st.st_size,st.st_uid,oct(stat.S_IMODE(st.st_mode)))==(r['device'],r['inode'],r['size'],r['owner'],r['mode']),'closure file/metadata changed')
 for path,before in snapshots.items():need(snapshot(pathlib.Path(path))==before,'alias/directory/interpreter resolution changed: '+path)
 for p in pdumps:need(sha(p)==expected[str(p.relative_to(root))],'pdmp changed')
 need(sha(tool)==toolpin and time.monotonic()<=deadline,'tool changed/deadline')
 summary={'static_candidate_complete':not missing and not issues,'elf_files':len(records),'eln_roots':149,'pdumps':dumps,'missing_sonames':sorted(missing),'issues':issues,'records':records,'edges':edges,'resolved_aliases':resolved_aliases,'path_snapshots':snapshots,'default_search_directories':[str(p) for p in allowed_dirs],'readelf_tool_sha256':toolpin,'gnu_executed':False,'runtime_qualified':False,'limitations':['fixed default host directories only, no ld.so.cache/hwcaps model','dlopen/NSS/locale/services not resolved','static global symbol union does not establish runtime binding order','ELN/dump byte identity not runtime compatibility proof']}
 with (out/'closure.json').open('x') as f:json.dump(summary,f,indent=2);f.write('\n')
 print(json.dumps({'static_candidate_complete':summary['static_candidate_complete'],'elf_files':len(records),'missing_sonames':sorted(missing),'issue_count':len(issues),'first_issues':issues[:3],'closure_sha256':sha(out/'closure.json'),'gnu_executed':False,'runtime_qualified':False}),flush=True)
 if missing or issues:raise SystemExit(1)
if __name__=='__main__':main()
