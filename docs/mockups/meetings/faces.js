// Stand-in camera pictures for the meeting mockups: an illustrated person in a
// room, drawn as SVG so the sheet needs no photographs. Each `.tl[data-p]`
// gets one; data-label adds the name pill, data-mic its state (on/off/talk),
// data-blur blurs the room the way "Blur my background" would.
const PEOPLE = {
  sanne:  { skin:'#eab894', hair:'#7a4a2a', style:'long',  shirt:'#35558a', bg:'office' },
  fatima: { skin:'#b07a58', hair:'#2a1a14', style:'hijab', hijab:'#7d3d52', shirt:'#5b3442', bg:'warm' },
  daan:   { skin:'#f0c7a8', hair:'#c8a167', style:'short', shirt:'#3e6a50', bg:'plants', beard:true },
  mei:    { skin:'#f2d1b5', hair:'#19120e', style:'bob',   shirt:'#8669b5', bg:'shelf' },
  you:    { skin:'#eec2a0', hair:'#4b3a2b', style:'short', shirt:'#2b3a41', bg:'window', glasses:true },
  joris:  { skin:'#6a432f', hair:'#140d0a', style:'curly', shirt:'#c46a33', bg:'kitchen' },
  bakker: { skin:'#e7bb9b', hair:'#a9a9a9', style:'side',  shirt:'#1d2328', bg:'hall', glasses:true, tie:'#8a2f3a' },
  anna:   { skin:'#f3d0b0', hair:'#b5542f', style:'long',  shirt:'#2f7f78', bg:'window' },
};
let uid = 0;
function shade(hex, f) {
  const n = parseInt(hex.slice(1), 16);
  const c = [n >> 16, (n >> 8) & 255, n & 255].map(v => Math.max(0, Math.min(255, Math.round(f < 0 ? v * (1 + f) : v + (255 - v) * f))));
  return '#' + c.map(v => v.toString(16).padStart(2, '0')).join('');
}
const leaf = (x, y, r, a, c) => `<ellipse cx="${x}" cy="${y}" rx="${r}" ry="${r * 0.42}" transform="rotate(${a} ${x} ${y})" fill="${c}"/>`;
function room(kind, id) {
  const wall = { office:['#46555b','#273136'], warm:['#6e5546','#35281f'], plants:['#45594d','#243029'], shelf:['#4b4759','#262331'],
                 window:['#5b666c','#2d3438'], kitchen:['#625c53','#2f2b26'], hall:['#3d4855','#1f252d'] }[kind];
  let s = `<defs><linearGradient id="w${id}" x1="0" y1="0" x2="0.3" y2="1"><stop offset="0" stop-color="${wall[0]}"/><stop offset="1" stop-color="${wall[1]}"/></linearGradient>
    <radialGradient id="g${id}" cx="0.2" cy="0.1" r="0.9"><stop offset="0" stop-color="#fff" stop-opacity=".16"/><stop offset=".6" stop-color="#fff" stop-opacity="0"/></radialGradient></defs>
    <rect width="320" height="180" fill="url(#w${id})"/>`;
  if (kind === 'office') s += `<rect x="214" y="14" width="96" height="100" rx="3" fill="#d6e6ea" opacity=".55"/><path d="M262 14v100M214 64h96" stroke="${wall[1]}" stroke-width="4"/>
    <rect x="20" y="126" width="34" height="40" rx="4" fill="#8a5a3c"/>${leaf(30,112,16,-40,'#3f7a4c')}${leaf(44,104,18,-70,'#4f8f5a')}${leaf(52,116,15,-10,'#36693f')}`;
  if (kind === 'warm') s += `<circle cx="56" cy="40" r="60" fill="#ffb565" opacity=".18"/><rect x="40" y="56" width="32" height="6" rx="3" fill="#d9a35c"/>
    <rect x="222" y="26" width="70" height="50" rx="3" fill="none" stroke="#c9a77f" stroke-width="3" opacity=".7"/><rect x="230" y="34" width="54" height="34" fill="#a1785a" opacity=".5"/>`;
  if (kind === 'plants') s += `${leaf(28,70,26,-50,'#4c8a57')}${leaf(16,104,24,-20,'#3b7046')}${leaf(44,92,22,-80,'#5d9c63')}
    ${leaf(292,64,26,50,'#4c8a57')}${leaf(304,100,24,20,'#3b7046')}${leaf(276,90,20,80,'#5d9c63')}<rect x="270" y="130" width="40" height="50" rx="5" fill="#b87a52"/>`;
  if (kind === 'shelf') { s += `<rect x="206" y="58" width="112" height="5" fill="#2a2433"/><rect x="206" y="108" width="112" height="5" fill="#2a2433"/>`;
    [['#c06630',14],['#2c917c',10],['#e2c46a',12],['#4374c4',9],['#b84868',13],['#c9c3b5',11],['#5f8a2e',10]].forEach(([c,w],i)=>{ s += `<rect x="${212+i*14}" y="${30+(i%3)*3}" width="${w-2}" height="${28-(i%3)*3}" fill="${c}" opacity=".85"/><rect x="${214+i*13}" y="${82+(i%2)*4}" width="${w-3}" height="${26-(i%2)*4}" fill="${c}" opacity=".7"/>`; }); }
  if (kind === 'window') s += `<rect x="8" y="10" width="110" height="130" rx="3" fill="#e9f1f2" opacity=".62"/><path d="M63 10v130M8 75h110" stroke="${wall[1]}" stroke-width="4"/><rect x="0" y="0" width="140" height="180" fill="#fff" opacity=".05"/>`;
  if (kind === 'kitchen') s += `<rect x="0" y="0" width="320" height="56" fill="#3f3a33"/><path d="M80 0v56M160 0v56M240 0v56" stroke="#2c2924" stroke-width="3"/><rect x="0" y="120" width="320" height="8" fill="#8b7d68"/><circle cx="270" cy="96" r="12" fill="#c9c3b5" opacity=".6"/>`;
  if (kind === 'hall') s += `<rect x="0" y="0" width="320" height="180" fill="#000" opacity=".1"/>${[0,1,2,3,4,5].map(i=>`<rect x="${8+i*18}" y="20" width="12" height="150" fill="#5a4636" opacity=".55"/>`).join('')}
    <path d="M276 10v120" stroke="#9a9a9a" stroke-width="2"/><path d="M276 12h34v14h-34z" fill="#ae1c28"/><path d="M276 26h34v14h-34z" fill="#fff"/><path d="M276 40h34v14h-34z" fill="#21468b"/>`;
  return s + `<rect width="320" height="180" fill="url(#g${id})"/>`;
}
function person(p, id, talking) {
  const sk = p.skin, hd = p.hair, dk = shade(sk, -0.18);
  let back = '', front = '';
  if (p.style === 'long') back = `<path d="M124 82C118 44 142 34 160 34C182 34 204 46 196 84C202 114 206 140 198 158L122 158C114 140 118 112 124 82Z" fill="${hd}"/>`;
  if (p.style === 'bob') back = `<path d="M124 84C118 44 142 34 160 34C182 34 204 46 196 86C198 104 196 116 190 122L130 122C124 116 122 104 124 84Z" fill="${hd}"/>`;
  if (p.style === 'hijab') back = `<path d="M120 86C118 44 140 30 160 30C180 30 202 44 200 86C202 112 216 132 238 160L82 160C104 132 118 112 120 86Z" fill="${p.hijab}"/><path d="M120 86C118 44 140 30 160 30C180 30 202 44 200 86" fill="none" stroke="${shade(p.hijab,.15)}" stroke-width="3" opacity=".6"/>`;
  if (p.style === 'short') front = `<path d="M130 80C127 50 144 40 160 40C178 40 194 50 190 80C187 64 178 56 160 57C145 57 134 64 130 80Z" fill="${hd}"/>`;
  if (p.style === 'long') front = `<path d="M131 76C132 50 148 41 162 41C178 41 191 53 189 76C180 60 166 55 150 59C141 62 135 68 131 76Z" fill="${hd}"/>`;
  if (p.style === 'bob') front = `<path d="M130 78C130 50 146 40 160 40C176 40 191 50 190 80C186 70 182 62 174 58C162 62 146 62 136 64C133 68 131 73 130 78Z" fill="${hd}"/>`;
  if (p.style === 'curly') { for (let a = -205; a <= 25; a += 23) { const r = a * Math.PI / 180; front += `<circle cx="${160 + 28 * Math.cos(r)}" cy="${66 + 26 * Math.sin(r)}" r="10" fill="${hd}"/>`; } front += `<ellipse cx="160" cy="50" rx="26" ry="13" fill="${hd}"/>`; }
  if (p.style === 'side') front = `<path d="M131 86C129 70 132 62 137 58L139 82ZM189 86C191 70 188 62 183 58L181 82Z" fill="${hd}"/>`;
  const hij = p.style === 'hijab';
  const rx = hij ? 25 : 29, ry = hij ? 31 : 35;
  let s = back;
  // torso, collar, neck
  s += `<path d="M58 182C64 140 104 124 160 122C216 124 256 140 262 182Z" fill="${p.shirt}"/>
        <path d="M58 182C64 140 104 124 160 122C216 124 256 140 262 182Z" fill="url(#t${id})"/>
        <path d="M146 100h28v22c-8 7-20 7-28 0z" fill="${dk}"/>`;
  if (p.tie) s += `<path d="M138 124L160 150L182 124" fill="#e9ecef"/><path d="M156 128h8l3 6-5 26h-4l-5-26z" fill="${p.tie}"/>`;
  else if (!hij) s += `<path d="M144 124L160 138L176 124" fill="none" stroke="${shade(p.shirt,.18)}" stroke-width="3"/>`;
  if (hij) s += `<path d="M128 104C134 124 148 132 160 132C172 132 186 124 192 104" fill="${p.hijab}"/>`;
  // head
  if (!hij) s += `<ellipse cx="131" cy="86" rx="5" ry="8" fill="${dk}"/><ellipse cx="189" cy="86" rx="5" ry="8" fill="${dk}"/>`;
  s += `<ellipse cx="160" cy="${hij ? 84 : 80}" rx="${rx}" ry="${ry}" fill="${sk}"/>
        <ellipse cx="160" cy="${hij ? 84 : 80}" rx="${rx}" ry="${ry}" fill="url(#f${id})"/>`;
  s += front;
  // features
  const ey = hij ? 84 : 83;
  s += `<path d="M143 ${ey - 9}q6-3 12 0M165 ${ey - 9}q6-3 12 0" stroke="${shade(hd,-.1)}" stroke-width="2.4" fill="none" stroke-linecap="round"/>
        <ellipse cx="149" cy="${ey}" rx="2.6" ry="3" fill="#24180f"/><ellipse cx="171" cy="${ey}" rx="2.6" ry="3" fill="#24180f"/>
        <path d="M160 ${ey + 3}q-3 8 1 10" stroke="${shade(sk,-.35)}" stroke-width="1.6" fill="none" stroke-linecap="round" opacity=".7"/>
        <circle cx="144" cy="${ey + 13}" r="6" fill="#e2725f" opacity=".14"/><circle cx="176" cy="${ey + 13}" r="6" fill="#e2725f" opacity=".14"/>`;
  if (p.beard) s += `<path d="M133 ${ey + 5}C136 116 148 124 160 124C172 124 184 116 187 ${ey + 5}C182 104 172 109 160 109C148 109 138 104 133 ${ey + 5}Z" fill="${hd}" opacity=".92"/><path d="M151 ${ey + 15}q9-4 18 0" stroke="${shade(hd,-.2)}" stroke-width="3" fill="none"/>`;
  s += talking
    ? `<ellipse cx="160" cy="${ey + 19}" rx="6.5" ry="4.2" fill="#5a2620"/><path d="M154 ${ey + 17}h12" stroke="#fff" stroke-width="1.6" opacity=".8"/>`
    : `<path d="M152 ${ey + 18}q8 5 16 0" stroke="#7a3b32" stroke-width="2.2" fill="none" stroke-linecap="round"/>`;
  if (p.glasses) s += `<g fill="#fff" fill-opacity=".07" stroke="#16191b" stroke-width="2"><rect x="139" y="${ey - 7}" width="18" height="14" rx="5"/><rect x="163" y="${ey - 7}" width="18" height="14" rx="5"/></g><path d="M157 ${ey - 2}h6" stroke="#16191b" stroke-width="2"/>`;
  return s;
}
function face(key, { blur = false, talking = false } = {}) {
  const p = PEOPLE[key], id = ++uid;
  return `<svg class="face" viewBox="0 0 320 180" preserveAspectRatio="xMidYMid slice">
    <defs>
      <filter id="b${id}" x="-10%" y="-10%" width="120%" height="120%"><feGaussianBlur stdDeviation="${blur ? 9 : 1.4}"/></filter>
      <linearGradient id="f${id}" x1="0" x2="1"><stop offset=".45" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".16"/></linearGradient>
      <linearGradient id="t${id}" x1="0" x2="1"><stop offset="0" stop-color="#fff" stop-opacity=".08"/><stop offset=".6" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".22"/></linearGradient>
      <radialGradient id="v${id}" cx=".5" cy=".45" r=".75"><stop offset=".55" stop-color="#000" stop-opacity="0"/><stop offset="1" stop-color="#000" stop-opacity=".42"/></radialGradient>
    </defs>
    <g filter="url(#b${id})">${room(p.bg, id)}</g>
    ${person(p, id, talking)}
    <rect width="320" height="180" fill="url(#v${id})"/>
  </svg>`;
}
const I = n => `<svg class="i"><use href="#${n}"/></svg>`;
document.querySelectorAll('.tl[data-p]').forEach(el => {
  const mic = el.dataset.mic || 'off';
  el.insertAdjacentHTML('afterbegin', face(el.dataset.p, { blur: 'blur' in el.dataset, talking: mic === 'talk' }));
  if (mic === 'talk') el.classList.add('talk');
  if (el.dataset.label) {
    const m = mic === 'off' ? `<span class="mm off">${I('micoff')}</span>`
      : mic === 'talk' ? `<span class="wv"><i style="height:5px"></i><i style="height:11px"></i><i style="height:7px"></i><i style="height:12px"></i></span>`
      : `<span class="mm">${I('mic')}</span>`;
    el.insertAdjacentHTML('beforeend', `<div class="shade"></div><div class="lb">${m}${el.dataset.label}</div>`);
  }
});
