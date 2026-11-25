import struct,os,json,sys
p=os.path.expanduser('~/.local/share/Steam/appcache/appinfo.vdf')
d=open(p,'rb').read()
magic,uni=struct.unpack_from('<II',d,0)
so,=struct.unpack_from('<q',d,8); off=16
n,=struct.unpack_from('<I',d,so); s=so+4; strtab=[]
for _ in range(n):
    e=d.index(b'\0',s); strtab.append(d[s:e].decode('utf-8','replace')); s=e+1
def rd(o):
    out={}
    while True:
        t=d[o];o+=1
        if t==8: return out,o
        ki,=struct.unpack_from('<I',d,o);o+=4;k=strtab[ki]
        if t==0: v,o=rd(o)
        elif t==1: e=d.index(b'\0',o);v=d[o:e].decode('utf-8','replace');o=e+1
        elif t==2: v,=struct.unpack_from('<i',d,o);o+=4
        elif t==7: v,=struct.unpack_from('<Q',d,o);o+=8
        elif t==3: v,=struct.unpack_from('<f',d,o);o+=4
        else: raise Exception('type %d'%t)
        out[k]=v
apps={}
while True:
    aid,=struct.unpack_from('<I',d,off)
    if aid==0: break
    size,=struct.unpack_from('<I',d,off+4)
    body=off+8+4+4+8+20+4+20
    try:
        v,_=rd(body); c=v.get('appinfo',v).get('common',{})
        apps[aid]={'name':c.get('name'),'type':(c.get('type') or '').lower(),'icon':c.get('clienticon') or c.get('icon')}
    except Exception as e: pass
    off=off+8+size
json.dump(apps,open(sys.argv[1],'w'))
print(len(apps))
