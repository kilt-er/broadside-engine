import * as THREE from 'three';
import fs from 'fs';

// ---- load the Aegis design from the v2 library ----
const lib = JSON.parse(fs.readFileSync('lib.json','utf8'));
const ship = lib.ships.find(s=>s.name==='Aegis');
const D = ship.design, S = D.settings;
console.log(`Loaded "${ship.name}"  plan:${D.plan.length}pts  section:${D.section.length}pts  engines:${S.engines.length}`);

// ---- globals the verbatim loft functions expect ----
const PLAN = D.plan, SECTION = D.section, HEIGHTPROF = D.heightProfile;
const ST = {
  stretch:S.stretch, hscale:S.hscale, wscale:S.wscale, noseTaper:S.noseTaper,
  secn:S.secn, engines:S.engines, engineSymZ:S.engineSymZ, engineSymY:S.engineSymY
};
const MAT_COLORS={ hull:0xb4c6e0, hullDark:0x6f88ad, tower:0xc8d8ee, dark:0x3c4a64,
  canopy:0x5fd8ff, gun:0xff8a48, batt:0xffd86b, engine:0x6fe0ff };

// ===== VERBATIM helpers from loft.html =====
function lerp(a,b,t){return a+(b-a)*t;}
function sampleHeightProf(x){
  if(!HEIGHTPROF||HEIGHTPROF.length<2) return 1.0;
  const n=HEIGHTPROF.length;
  let i=0; while(i<n-2 && HEIGHTPROF[i+1][0]<x) i++;
  const a=HEIGHTPROF[i], b=HEIGHTPROF[Math.min(n-1,i+1)];
  const t=(b[0]-a[0])?(x-a[0])/(b[0]-a[0]):0;
  return Math.max(0.05, lerp(a[1],b[1],Math.max(0,Math.min(1,t))));
}
function effectiveSection(){
  const top = SECTION.filter(p=>p[1]>=-1e-9);
  const isTopHalf = SECTION.length>=2 && top.length===SECTION.length && SECTION.some(p=>p[1]>1e-3);
  if(!isTopHalf) return SECTION;
  if(top.length<2) return SECTION;
  const bottom = top.slice().reverse().map(p=>[p[0], -p[1]]).filter(p=>Math.abs(p[1])>1e-6);
  return top.concat(bottom);
}
function sectionMirrored(){
  const SEC = SECTION;
  return SEC.length>=2 && SEC.every(p=>p[0]>=-1e-4) && SEC.some(p=>p[0]>1e-3);
}
function sampleSection(t){
  const SEC=effectiveSection();
  const n=SEC.length; const f=t*(n-1); const i=Math.min(n-2,Math.floor(f)); const u=f-i;
  return [ lerp(SEC[i][0],SEC[i+1][0],u), lerp(SEC[i][1],SEC[i+1][1],u) ];
}
function dedupeStationsByX(stations){
  const byX=new Map();
  for(const st of stations){
    const key=st.x.toFixed(4);
    const prev=byX.get(key);
    if(!prev || st.w>prev.w) byX.set(key, st);
  }
  return Array.from(byX.values()).sort((a,b)=>a.x-b.x);
}
// ===== VERBATIM buildHull from loft.html =====
function buildHull(){
  const L=1.0*ST.stretch, H=ST.hscale;
  let PLAN_EFF = PLAN;
  { const isFrontHalf = PLAN.length>=2 && PLAN.every(p=>p[0]>=0.5-1e-4) && PLAN.some(p=>p[0]>0.5+1e-3);
    if(isFrontHalf){
      const front = PLAN.filter(p=>p[0]>=0.5-1e-9).slice().sort((a,b)=>a[0]-b[0]);
      if(front.length>=2){
        const back = front.slice().reverse().map(p=>[1-p[0], p[1]]).filter(p=>p[0]<0.5-1e-9);
        PLAN_EFF = back.concat(front);
      }
    }
  }
  const stationsRaw=PLAN_EFF.map(p=>({x:(p[0]-0.5)*2*L, w:Math.abs(p[1]), hm:sampleHeightProf(p[0]), px:p[0]}));
  const stations=dedupeStationsByX(stationsRaw);
  const SECN=Math.max(3,Math.min(40,Math.round(ST.secn||10)));
  const verts=[], idx=[];
  const ringsCount=stations.length;
  function noseHeightScale(px){
    const nt=ST.noseTaper||0;
    if(nt<=0) return 1;
    const start=0.4;
    if(px<=start) return 1;
    const t=(px-start)/(1-start);
    const eased=t*t;
    return lerp(1, 1-nt, eased);
  }
  function ringPts(st){
    const pts=[];
    const hh=H*st.hm*noseHeightScale(st.px);
    const ww=(Number.isFinite(ST.wscale)?ST.wscale:1);
    if(sectionMirrored()){
      for(let s=0;s<SECN;s++){ const [zf,y]=sampleSection(s/(SECN-1)); pts.push([st.x, y*hh, zf*st.w*ww]); }
      for(let s=SECN-2;s>=1;s--){ const [zf,y]=sampleSection(s/(SECN-1)); pts.push([st.x, y*hh, -zf*st.w*ww]); }
    } else {
      for(let s=0;s<SECN;s++){ const [zf,y]=sampleSection(s/(SECN-1)); pts.push([st.x, y*hh, zf*st.w*ww]); }
    }
    return pts;
  }
  const rings=stations.map(ringPts);
  const N=rings[0].length;
  rings.forEach(r=>r.forEach(v=>verts.push(...v)));
  for(let s=0;s<ringsCount-1;s++){ const a=s*N,b=(s+1)*N;
    for(let i=0;i<N;i++){const j=(i+1)%N; idx.push(a+i,a+j,b+i); idx.push(a+j,b+j,b+i);} }
  function addCap(ringIdx, flip){
    const base=ringIdx*N;
    let cx=0,cy=0,cz=0;
    for(let i=0;i<N;i++){ cx+=verts[(base+i)*3]; cy+=verts[(base+i)*3+1]; cz+=verts[(base+i)*3+2]; }
    cx/=N; cy/=N; cz/=N;
    const cIdx=verts.length/3; verts.push(cx,cy,cz);
    for(let i=0;i<N;i++){ const j=(i+1)%N;
      if(flip) idx.push(cIdx, base+j, base+i); else idx.push(cIdx, base+i, base+j); }
  }
  addCap(0, true);
  addCap(ringsCount-1, false);
  const g=new THREE.BufferGeometry();
  g.setAttribute('position',new THREE.Float32BufferAttribute(verts,3));
  g.setIndex(idx); g.computeVertexNormals();
  return {geo:g, L, H, stations, rings, SECN};
}

// ===== build hull + engines into material-keyed pieces =====
const pieces=[];
const hull=buildHull();
pieces.push({geo:hull.geo, matKey:'hull'});

// stern context (verbatim math)
const H=hull.H;
const stations=hull.stations, rings=hull.rings;
const sternStation = stations.reduce((a,b)=>b.x<a.x?b:a, stations[0]);
const sternX = sternStation.x;
const sternRing = rings[0];
let sternZmax=0, sternYtop=0, sternYbot=0;
for(const v of sternRing){ sternZmax=Math.max(sternZmax,Math.abs(v[2])); sternYtop=Math.max(sternYtop,v[1]); sternYbot=Math.min(sternYbot,v[1]); }
sternZmax=Math.max(0.05,sternZmax);
const sternYmax=Math.max(0.05, Math.max(Math.abs(sternYtop),Math.abs(sternYbot)));
const baseBellR=H*0.34, baseBellLen=H*0.3;
const engList=[];
(ST.engines||[]).forEach(e=>{
  const ey=(e.y||0);
  const variants=[[e.z,ey]];
  if(ST.engineSymZ && Math.abs(e.z)>1e-3) variants.push([-e.z,ey]);
  if(ST.engineSymY && Math.abs(ey)>1e-3){ const cur=variants.slice(); cur.forEach(([vz,vy])=>variants.push([vz,-vy])); }
  variants.forEach(([vz,vy])=>engList.push({z:vz,y:vy,r:e.r,len:e.len}));
});
function bakedCyl(rt,rb,len,segs, rotZ, px,py,pz){
  const g=new THREE.CylinderGeometry(rt,rb,len,segs);
  const m=new THREE.Object3D(); m.rotation.z=rotZ; m.position.set(px,py,pz); m.updateMatrixWorld(true);
  g.applyMatrix4(m.matrixWorld);
  return g;
}
engList.forEach(e=>{
  const z=e.z*sternZmax, y=(e.y||0)*sternYmax, r=baseBellR*(e.r||1), bl=baseBellLen*(e.len||1);
  pieces.push({geo:bakedCyl(r*0.82,r,bl,8, Math.PI/2, sternX-bl*0.5,y,z), matKey:'dark'});      // bell housing
  pieces.push({geo:bakedCyl(r*0.7,r*0.7,H*0.06,8, Math.PI/2, sternX-bl,y,z), matKey:'engine'});  // glow disc
});
console.log(`pieces: hull + ${engList.length*2} engine parts (${engList.length} bells expanded from ${S.engines.length})`);

// ===== material table (loft editor slots; engine = unlit glow) =====
const MATS={
  hull:{col:MAT_COLORS.hull}, hullDark:{col:MAT_COLORS.hullDark}, tower:{col:MAT_COLORS.tower},
  dark:{col:MAT_COLORS.dark}, canopy:{col:MAT_COLORS.canopy,emissive:0x0a2230},
  gun:{col:MAT_COLORS.gun,emissive:0x331100}, batt:{col:MAT_COLORS.batt,emissive:0x221800},
  engine:{col:MAT_COLORS.engine, basic:true}
};

// ===== exporter (CAD buildGLB/packGLB, adapted: extras=§3, fs output) =====
function srgbToLinear(c){ return c<=0.04045 ? c/12.92 : Math.pow((c+0.055)/1.055,2.4); }
function hexToLinearRGB(hex){ const r=((hex>>16)&255)/255,g=((hex>>8)&255)/255,b=(hex&255)/255; return [srgbToLinear(r),srgbToLinear(g),srgbToLinear(b)]; }
function pad4(n){ return (n+3)&~3; }
function buildGLB(pieces, extras){
  const byMat=new Map();
  for(const p of pieces){ if(!byMat.has(p.matKey)) byMat.set(p.matKey,[]); byMat.get(p.matKey).push(p.geo); }
  const TARGET_LEN=12;
  let rMin=[1e9,1e9,1e9], rMax=[-1e9,-1e9,-1e9];
  for(const p of pieces){ const pos=p.geo.getAttribute('position');
    for(let i=0;i<pos.count;i++){ const x=pos.getX(i),y=pos.getY(i),z=pos.getZ(i);
      rMin[0]=Math.min(rMin[0],x);rMin[1]=Math.min(rMin[1],y);rMin[2]=Math.min(rMin[2],z);
      rMax[0]=Math.max(rMax[0],x);rMax[1]=Math.max(rMax[1],y);rMax[2]=Math.max(rMax[2],z); } }
  const ctr=[(rMin[0]+rMax[0])/2,(rMin[1]+rMax[1])/2,(rMin[2]+rMax[2])/2];
  const rawLen=Math.max(1e-6, rMax[0]-rMin[0]); const SCALE=TARGET_LEN/rawLen;
  const xform=(x,y,z)=>[ (x-ctr[0])*SCALE, (y-ctr[1])*SCALE, (z-ctr[2])*SCALE ];
  const accessors=[], bufferViews=[], meshes=[], materials=[]; let byteOffset=0; const chunks=[];
  function addBufferView(ta, target){ const bytes=ta.byteLength; const bv={buffer:0,byteOffset,byteLength:bytes}; if(target)bv.target=target; bufferViews.push(bv);
    chunks.push(ta.buffer.slice(ta.byteOffset, ta.byteOffset+bytes)); const padded=pad4(bytes); if(padded>bytes) chunks.push(new ArrayBuffer(padded-bytes)); byteOffset+=padded; return bufferViews.length-1; }
  let gMin=[1e9,1e9,1e9], gMax=[-1e9,-1e9,-1e9];
  for(const [matKey,geos] of byMat){
    let posArr=[], nrmArr=[], idxArr=[], base=0;
    for(const g of geos){ const pos=g.getAttribute('position'); const nrm=g.getAttribute('normal'); const idx=g.getIndex();
      for(let i=0;i<pos.count;i++){ const [x,y,z]=xform(pos.getX(i),pos.getY(i),pos.getZ(i)); posArr.push(x,y,z);
        nrmArr.push(nrm?nrm.getX(i):0, nrm?nrm.getY(i):1, nrm?nrm.getZ(i):0);
        gMin[0]=Math.min(gMin[0],x);gMin[1]=Math.min(gMin[1],y);gMin[2]=Math.min(gMin[2],z);
        gMax[0]=Math.max(gMax[0],x);gMax[1]=Math.max(gMax[1],y);gMax[2]=Math.max(gMax[2],z); }
      if(idx){ for(let i=0;i<idx.count;i++) idxArr.push(base+idx.getX(i)); } else { for(let i=0;i<pos.count;i++) idxArr.push(base+i); }
      base+=pos.count; }
    const positions=new Float32Array(posArr), normals=new Float32Array(nrmArr), indices=new Uint32Array(idxArr);
    let pmin=[1e9,1e9,1e9], pmax=[-1e9,-1e9,-1e9];
    for(let i=0;i<positions.length;i+=3){ for(let k=0;k<3;k++){ pmin[k]=Math.min(pmin[k],positions[i+k]); pmax[k]=Math.max(pmax[k],positions[i+k]); } }
    const bvPos=addBufferView(positions,34962), bvNrm=addBufferView(normals,34962), bvIdx=addBufferView(indices,34963);
    const aPos=accessors.length; accessors.push({bufferView:bvPos,componentType:5126,count:positions.length/3,type:'VEC3',min:pmin,max:pmax});
    const aNrm=accessors.length; accessors.push({bufferView:bvNrm,componentType:5126,count:normals.length/3,type:'VEC3'});
    const aIdx=accessors.length; accessors.push({bufferView:bvIdx,componentType:5125,count:indices.length,type:'SCALAR'});
    const md=MATS[matKey]||MATS.hull; const lin=hexToLinearRGB(md.col);
    const mat={ name:matKey, pbrMetallicRoughness:{ baseColorFactor:[lin[0],lin[1],lin[2],1], metallicFactor:0, roughnessFactor:0.85 } };
    if(md.emissive){ const e=hexToLinearRGB(md.emissive); mat.emissiveFactor=[e[0],e[1],e[2]]; }
    if(md.basic){ mat.extensions={KHR_materials_unlit:{}}; mat.emissiveFactor=[lin[0],lin[1],lin[2]]; }
    const mi=materials.length; materials.push(mat);
    meshes.push({ name:matKey, primitives:[{ attributes:{POSITION:aPos, NORMAL:aNrm}, indices:aIdx, material:mi }] });
  }
  let binLen=0; for(const c of chunks) binLen+=c.byteLength;
  const binBuf=new Uint8Array(binLen); let o=0; for(const c of chunks){ binBuf.set(new Uint8Array(c),o); o+=c.byteLength; }
  const nodes=meshes.map((m,i)=>({mesh:i, name:m.name}));
  const gltf={ asset:{version:'2.0', generator:'Broadside Loft Editor (Aegis standalone)'},
    extensionsUsed: materials.some(m=>m.extensions&&m.extensions.KHR_materials_unlit)?['KHR_materials_unlit']:undefined,
    scene:0, scenes:[{ nodes:nodes.map((_,i)=>i), extras }],
    nodes, meshes, materials, accessors, bufferViews, buffers:[{ byteLength:binLen }] };
  if(!gltf.extensionsUsed) delete gltf.extensionsUsed;
  return { gltf, binBuf, bounds:{min:gMin,max:gMax}, scaleApplied:SCALE, rawLength:rawLen };
}
function packGLB(gltf, binBuf){
  const enc=new TextEncoder(); let jsonBytes=enc.encode(JSON.stringify(gltf));
  let jsonPad=pad4(jsonBytes.length)-jsonBytes.length;
  if(jsonPad){ const t=new Uint8Array(jsonBytes.length+jsonPad); t.set(jsonBytes); for(let i=0;i<jsonPad;i++)t[jsonBytes.length+i]=0x20; jsonBytes=t; }
  let binPad=pad4(binBuf.length)-binBuf.length; const binTotal=binBuf.length+binPad;
  const total=12 + 8+jsonBytes.length + 8+binTotal;
  const buf=new ArrayBuffer(total); const dv=new DataView(buf); const u8=new Uint8Array(buf); let p=0;
  dv.setUint32(p,0x46546C67,true);p+=4; dv.setUint32(p,2,true);p+=4; dv.setUint32(p,total,true);p+=4;
  dv.setUint32(p,jsonBytes.length,true);p+=4; dv.setUint32(p,0x4E4F534A,true);p+=4; u8.set(jsonBytes,p);p+=jsonBytes.length;
  dv.setUint32(p,binTotal,true);p+=4; dv.setUint32(p,0x004E4942,true);p+=4; u8.set(binBuf,p);p+=binBuf.length;
  return Buffer.from(buf);
}

// §3 look-params extras (+ laz/lel for today's engine)
const L0=S.lights&&S.lights[0]?S.lights[0]:{az:0,el:0};
const extras={
  laz:L0.az, lel:L0.el,                       // current engine reads these
  lights:S.lights, lightSym:S.lightSym,       // full 3-light (Tier-1.5)
  bands:S.bands, shadeModel:S.shadeModel,
  outlineWidth:S.outlineWidth, outlineCorner:(S.outlineCorner||'sharp'),
  grade:D.grade
};
const built=buildGLB(pieces, extras);
const glb=packGLB(built.gltf, built.binBuf);
fs.writeFileSync('/mnt/user-data/outputs/Aegis.glb', glb);
const b=built.bounds;
console.log(`\nGLB written: ${glb.length} bytes`);
console.log(`scale applied: ${built.scaleApplied.toFixed(4)} (raw length ${built.rawLength.toFixed(3)} -> 12)`);
console.log(`post-scale bounds  X[${b.min[0].toFixed(2)},${b.max[0].toFixed(2)}] Y[${b.min[1].toFixed(2)},${b.max[1].toFixed(2)}] Z[${b.min[2].toFixed(2)},${b.max[2].toFixed(2)}]`);
console.log(`materials: ${built.gltf.materials.map(m=>m.name).join(', ')}`);
console.log(`extras keys: ${Object.keys(extras).join(', ')}`);
