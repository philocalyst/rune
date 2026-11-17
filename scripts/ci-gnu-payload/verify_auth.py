#!/usr/bin/env python3
"""Verify one fixed Debian metadata snapshot; no downloads or package execution.
Requires exclusive private directories and external bounded supervisor/tool pins.
"""
import datetime,email.utils,hashlib,json,lzma,os,pathlib,stat,subprocess,sys
HERE=pathlib.Path(__file__).resolve().parent
KEY='506b815cbb32d9b6066b4a2aa524071e071761e7e7f68c3ac74f3061ba852017'
PACKAGES='eb1f62d2d894419f8792a52f1b41db7fecf0b761bcb24ac52a987be767797441'
RELEASE='933990236b8b8a1e39957362d361f92cd9fca8ccd13c0443f653762de1f70a4e'
INDEX='b74b5bbca6f4701df6c5db54793a68449fb267ac10a9314e761b972ac63b3760'
SIGNERS={('4CB50190207B4758A3F73A796ED0E7B82643E131','B8B80B5B623EAB6AD8775C45B7C5D7D6350947F8'),('B8E5F13176D2A7A75220028078DBA3BC47EF2265','04B54C3CDCA79751B16BC6B5225629DF75B188BD')}
def need(ok,message):
 if not ok:raise RuntimeError(message)
def read_pinned(p,pin,size=None):
 st=p.lstat();need(stat.S_ISREG(st.st_mode) and st.st_nlink==1,'not exclusive regular input')
 need(st.st_size<=2*1024*1024 and (size is None or st.st_size==size),'input size')
 b=p.read_bytes();need(hashlib.sha256(b).hexdigest()==pin,'input hash');return b
def private(p):
 need(p.is_absolute() and p.resolve()==p,'canonical private directory required');s=p.lstat()
 need(stat.S_ISDIR(s.st_mode) and s.st_uid==os.getuid() and stat.S_IMODE(s.st_mode)==0o700,'private directory owner/mode')
def main():
 need(len(sys.argv)==3,'METADATA_DIR OUTPUT_RECEIPT')
 meta=pathlib.Path(sys.argv[1]);out=pathlib.Path(sys.argv[2]);private(meta);private(out.parent)
 need(not out.exists() and not out.is_symlink(),'fresh receipt required')
 key=HERE/'debian-archive-keyring.gpg';read_pinned(key,KEY)
 quartet=json.loads(read_pinned(HERE/'packages.json',PACKAGES));release=read_pinned(meta/'InRelease',RELEASE,59432);index=read_pinned(meta/'Packages.xz',INDEX,300424)
 home=meta/'gnupg';home.mkdir(mode=0o700);private(home)
 # gpgv uses only the explicit public keyring, does not retrieve/import keys.
 # Logs are files, not unbounded captured memory. Outer RLIMIT_FSIZE/wall apply.
 status=meta/'gpgv.status';errors=meta/'gpgv.stderr'
 with status.open('xb') as s,errors.open('xb') as e:
  r=subprocess.run(['/usr/bin/gpgv','--homedir',str(home),'--keyring',str(key),'--status-fd','1',str(meta/'InRelease')],stdin=subprocess.DEVNULL,stdout=s,stderr=e,env={'PATH':'/usr/bin:/bin','LANG':'C','LC_ALL':'C'},close_fds=True,timeout=15)
 need(r.returncode==0,'signature verifier failed')
 need(status.stat().st_size<=65536 and errors.stat().st_size<=65536,'signature logs too large')
 records=status.read_text().splitlines();valid=[]
 for line in records:
  if line.startswith('[GNUPG:] VALIDSIG '):
   fields=line.split();need(len(fields)==12,'unexpected signature status');valid.append((fields[2],fields[-1]))
  need(not any(line.startswith('[GNUPG:] '+x+' ') for x in ('BADSIG','ERRSIG','EXPKEYSIG','EXPSIG','REVKEYSIG','NO_PUBKEY')),'invalid signature status')
 need(len(valid)==2 and set(valid)==SIGNERS,'expected both exact archive signatures')
 # Only parse the exact hashed and freshly authenticated cleartext.
 text=release.decode('ascii');need(text.startswith('-----BEGIN PGP SIGNED MESSAGE-----\nHash: SHA256\n\n'),'cleartext header')
 body=text.split('\n\n',1)[1].split('\n-----BEGIN PGP SIGNATURE-----',1)[0];lines=body.splitlines()
 def field(name):
  values=[l[len(name)+2:] for l in lines if l.startswith(name+': ')]
  need(len(values)==1,'duplicate/missing release field');return values[0]
 need(field('Origin')=='Debian Backports' and field('Codename')=='bookworm-backports' and field('Suite')=='oldstable-backports','wrong archive')
 start=email.utils.parsedate_to_datetime(field('Date'));end=email.utils.parsedate_to_datetime(field('Valid-Until'));now=datetime.datetime.now(datetime.timezone.utc)
 need(start<=now<end,'metadata expired/not yet valid')
 section=body.split('SHA256:\n',1)[1].split('\nSHA512:',1)[0]
 row=[l.split() for l in section.splitlines() if l.endswith(' main/binary-amd64/Packages.xz')]
 need(row==[[INDEX,'300424','main/binary-amd64/Packages.xz']],'signed index binding')
 dec=lzma.LZMADecompressor(memlimit=64*1024*1024);plain=dec.decompress(index,max_length=16*1024*1024+1)
 need(len(plain)<=16*1024*1024 and dec.eof and not dec.unused_data,'index bound/stream')
 selected=[];names={v['package'] for v in quartet}
 for stanza in plain.decode('utf-8').split('\n\n'):
  d={}
  for line in stanza.splitlines():
   if not line or line[0].isspace():continue
   k,sep,v=line.partition(': ');need(bool(sep) and k not in d,'malformed index field');d[k]=v
  if d.get('Package') in names:selected.append(d)
 need(len(selected)==4,'exact quartet required')
 for v in quartet:
  rows=[d for d in selected if d['Package']==v['package']];need(len(rows)==1,'duplicate package')
  d=rows[0];need(d['Version']==v['version'] and d['Architecture']==v['architecture'] and d['SHA256']==v['sha256'] and d['Size']==str(v['size']) and d['Filename']=='pool/main/e/emacs/'+v['filename'],'package binding mismatch')
 # Recheck metadata and trust root after subprocess and parsing.
 read_pinned(key,KEY);read_pinned(meta/'InRelease',RELEASE,59432);read_pinned(meta/'Packages.xz',INDEX,300424)
 now=datetime.datetime.now(datetime.timezone.utc);need(start<=now<end,'expired during verification')
 receipt={'authenticated':True,'inrelease_sha256':RELEASE,'packages_xz_sha256':INDEX,'keyring_sha256':KEY,'release_date':start.isoformat(sep=' '),'valid_until':end.isoformat(sep=' '),'verified_at':now.isoformat(),'signers':sorted(valid),'packages':selected,'package_code_executed':False}
 with out.open('x') as f:json.dump(receipt,f,indent=2);f.write('\n')
 out.chmod(0o600)
if __name__=='__main__':main()
