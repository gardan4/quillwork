/**
 * Default stylesheet. Every colour and size a host may want to change is a `--qw-*` custom
 * property on `.qw-root` (see README "Theming"). Document colours ride on private `--_c` / `--_bg`
 * properties so the dark theme can adjust them.
 */
export const css = `
.qw-root{
--qw-canvas:#f3f3f1;--qw-page:#fff;--qw-ink:#1f1f1d;--qw-ink-2:#6b6b66;--qw-rule:#dcdcd6;
--qw-accent:#2f5bd3;--qw-focus:var(--qw-accent);--qw-heading:#1f3864;
--qw-ins:#1a7f37;--qw-ins-bg:rgba(26,127,55,.08);--qw-del:#b42318;--qw-del-bg:rgba(180,35,24,.06);
--qw-fmt:#8250df;--qw-hl:rgba(255,200,0,.35);--qw-flash:rgba(47,91,211,.18);
--qw-comment:#e0a400;--qw-comment-bg:rgba(255,196,0,.2);--qw-comment-active:rgba(255,196,0,.45);
--qw-bubble-bg:#fff;--qw-shadow:0 1px 2px rgba(0,0,0,.08),0 2px 8px rgba(0,0,0,.06);
--qw-radius:8px;--qw-rail-width:260px;--qw-gap:24px;--qw-page-shadow:0 1px 3px rgba(0,0,0,.12);
--qw-doc-font:"Carlito","Calibri","Liberation Sans",Arial,sans-serif;
--qw-ui-font:system-ui,-apple-system,"Segoe UI",sans-serif;--qw-ui-size:13px;
--qw-table-rule:#c8c8c2;--qw-warn:#9a6700;--qw-warn-bg:rgba(212,167,44,.15);
--qw-author-0:#1a7f37;--qw-author-1:#2f5bd3;--qw-author-2:#8250df;--qw-author-3:#bc4c00;--qw-author-4:#0e7c86;--qw-author-5:#bf3989;
position:relative;container-type:inline-size;background:var(--qw-canvas);color:var(--qw-ink);
font-family:var(--qw-ui-font);font-size:var(--qw-ui-size);padding:var(--qw-gap) 16px;box-sizing:border-box}
.qw-root.qw-dark{
--qw-canvas:#161615;--qw-page:#232321;--qw-ink:#e6e6e1;--qw-ink-2:#a3a39c;--qw-rule:#3a3a37;
--qw-accent:#7aa2ff;--qw-heading:#9db7ef;--qw-ins:#56d364;--qw-ins-bg:rgba(86,211,100,.12);
--qw-del:#ff7b72;--qw-del-bg:rgba(255,123,114,.1);--qw-fmt:#c297ff;--qw-hl:rgba(255,200,0,.25);
--qw-flash:rgba(122,162,255,.22);--qw-comment:#e3b341;--qw-comment-bg:rgba(227,179,65,.18);
--qw-comment-active:rgba(227,179,65,.4);--qw-bubble-bg:#2b2b29;--qw-shadow:0 1px 2px rgba(0,0,0,.5);
--qw-page-shadow:0 1px 3px rgba(0,0,0,.6);--qw-table-rule:#4a4a46;--qw-warn:#e3b341;--qw-warn-bg:rgba(227,179,65,.15);
--qw-author-0:#56d364;--qw-author-1:#7aa2ff;--qw-author-2:#c297ff;--qw-author-3:#ffa657;--qw-author-4:#39c5cf;--qw-author-5:#ff7eb6;
color-scheme:dark}
.qw-root *,.qw-root *::before,.qw-root *::after{box-sizing:border-box}
.qw-bar{display:flex;flex-wrap:wrap;gap:6px;margin:0 auto 12px;max-width:100%}
.qw-chip{border:1px solid var(--qw-rule);border-radius:999px;padding:1px 8px;color:var(--qw-ink-2)}
.qw-chip-warn{border-color:transparent;background:var(--qw-warn-bg);color:var(--qw-warn)}
.qw-main{display:flex;gap:var(--qw-gap);justify-content:center;align-items:flex-start}
.qw-doc{flex:0 1 auto;min-width:0;max-width:100%;display:flex;flex-direction:column;gap:var(--qw-gap)}
.qw-sec{background:var(--qw-page);box-shadow:var(--qw-page-shadow);max-width:100%;display:flex;flex-direction:column;
font-family:var(--qw-doc-font);font-size:11pt;line-height:1.15;color:var(--qw-ink);overflow-wrap:break-word}
.qw-sec.qw-cont{margin-top:calc(-1 * var(--qw-gap));box-shadow:none;border-top:1px dashed var(--qw-rule);min-height:0!important}
.qw-hdr,.qw-ftr{color:var(--qw-ink-2);font-size:.95em}
.qw-hdr{display:flex;flex-direction:column;justify-content:flex-start}
.qw-ftr{display:flex;flex-direction:column;justify-content:flex-end}
.qw-body{flex:1}
.qw-p{margin:0 0 8pt;min-height:1lh;white-space:pre-wrap;tab-size:4}
.qw-h{font-weight:600;color:var(--qw-heading);margin:12pt 0 4pt;line-height:1.15}
.qw-h1{font-size:16pt}.qw-h2{font-size:13pt}.qw-h3{font-size:12pt}.qw-h4,.qw-h5,.qw-h6{font-size:11pt}.qw-h4{font-style:italic}
.qw-label{display:inline-block;min-width:1.6em;padding-inline-end:.4em;user-select:none}
.qw-tab{display:inline-block;min-width:2em;white-space:pre}
.qw-pb,.qw-colb{display:block;height:0;margin:6pt 0;border-top:1px dashed var(--qw-rule);position:relative}
.qw-pb::after,.qw-colb::after,.qw-pbb::before,.qw-sect-end::after{position:absolute;left:50%;transform:translate(-50%,-50%);
font:10px var(--qw-ui-font);color:var(--qw-ink-2);background:var(--qw-page);padding:0 6px}
.qw-pb::after{content:"Page break"}.qw-colb::after{content:"Column break"}
.qw-pbb{position:relative;margin-top:28px!important}
.qw-pbb::before{content:"Page break";top:-16px;border:0}
.qw-pbb::after{content:"";position:absolute;left:0;right:0;top:-16px;border-top:1px dashed var(--qw-rule);z-index:-1}
.qw-sect-end{position:relative;padding-bottom:14px}
.qw-sect-end::after{content:"Section break";bottom:0}
.qw-body>.qw-sect-end:last-child{padding-bottom:0}.qw-body>.qw-sect-end:last-child::after{content:none}
.qw-c{color:var(--_c)}
.qw-bg{background-color:var(--_bg)}
.qw-dark .qw-c-dk{color:inherit}
.qw-dark .qw-bg:not(.qw-hlt){background-color:color-mix(in srgb,var(--_bg) 22%,var(--qw-page))}
.qw-hlt{color:#111}
.qw-link{color:var(--qw-accent);text-decoration:underline}
.qw-p a{color:var(--qw-accent)}
.qw-field{background:color-mix(in srgb,var(--qw-ink) 6%,transparent);border-radius:2px}
.qw-sdt{box-shadow:inset 0 -1px 0 color-mix(in srgb,var(--qw-accent) 50%,transparent)}
.qw-sdt-block{outline:1px dotted color-mix(in srgb,var(--qw-accent) 40%,transparent);outline-offset:2px}
.qw-noteref{font-size:.7em;line-height:0}
.qw-noteref a{color:var(--qw-accent);text-decoration:none}
.qw-f-hidden{opacity:.55;text-decoration:underline dotted var(--qw-ink-2)}
.qw-nohidden .qw-f-hidden{display:none}
.qw-f-white,.qw-f-tiny,.qw-f-zw{outline:1px dashed var(--qw-warn);background:var(--qw-warn-bg)}
.qw-empty{user-select:none}
.qw-rev{--_rc:var(--qw-ins);border-radius:2px;cursor:pointer}
.qw-ins{color:var(--_rc);text-decoration:underline;text-decoration-color:var(--_rc);text-decoration-thickness:1px;background:var(--qw-ins-bg);text-decoration-skip-ink:none}
.qw-del{color:var(--_rc);text-decoration:line-through;text-decoration-color:var(--_rc);opacity:.85}
ins.qw-rev,del.qw-rev{display:inline}
.qw-move.qw-ins{text-decoration-style:double}.qw-move.qw-del{text-decoration-style:double}
.qw-fchg{box-shadow:inset 0 -2px 0 color-mix(in srgb,var(--qw-fmt) 45%,transparent)}
.qw-pchg{border-inline-start:2px solid var(--qw-fmt);padding-inline-start:4px}
.qw-pmark{font-size:.85em;user-select:none}
.qw-p-ins,.qw-p-del{border-inline-start:2px solid var(--qw-ins);padding-inline-start:4px}
.qw-p-del{border-color:var(--qw-del)}
.qw-row-ins>td,.qw-row-ins>th{background:var(--qw-ins-bg)}
.qw-row-del>td,.qw-row-del>th{background:var(--qw-del-bg);text-decoration:line-through}
figure.qw-ins,figure.qw-del{outline:2px solid var(--_rc);outline-offset:2px}
.qw-rev:focus-visible,.qw-img:focus-visible,.qw-bubble:focus-visible,.qw-card:focus-visible,.qw-btn:focus-visible{outline:2px solid var(--qw-focus);outline-offset:2px}
.qw-rev-active{box-shadow:0 0 0 2px color-mix(in srgb,var(--_rc) 35%,transparent)}
.qw-cm{background:var(--qw-comment-bg);border-bottom:2px solid var(--qw-comment);cursor:pointer}
.qw-cm-done{background:none;border-bottom:1px dotted var(--qw-ink-2)}
.qw-cm-active{background:var(--qw-comment-active)}
.qw-table{border-collapse:collapse;margin:0 0 8pt;max-width:100%;table-layout:fixed}
.qw-cell{border:1px solid var(--qw-table-rule);padding:2pt 5.4pt;vertical-align:top;text-align:start;font-weight:inherit}
.qw-cell>.qw-p:last-child{margin-bottom:0}
.qw-table caption{caption-side:top;text-align:start;color:var(--qw-ink-2);padding-bottom:4px}
.qw-img{display:block;position:relative;overflow:hidden;max-width:100%;margin:0 0 8pt;cursor:pointer;border-radius:1px}
span.qw-img{display:inline-block;vertical-align:bottom;margin:0}
.qw-img>img{display:block;width:100%;height:100%;object-fit:fill}
.qw-img>img.qw-crop{position:absolute;max-width:none}
.qw-align-center{margin-inline:auto}.qw-align-right{margin-inline-start:auto}
.qw-float-left{float:left;margin:0 12px 6px 0}.qw-float-right{float:right;margin:0 0 6px 12px}
.qw-anchored{opacity:.9}
.qw-ph{display:flex;align-items:center;justify-content:center;width:100%;height:100%;min-height:24px;padding:4px;
background:repeating-linear-gradient(45deg,transparent 0 6px,color-mix(in srgb,var(--qw-ink) 5%,transparent) 6px 12px);
border:1px dashed var(--qw-rule);color:var(--qw-ink-2);font:11px var(--qw-ui-font);text-align:center}
.qw-selected{outline:2px solid var(--qw-accent);outline-offset:2px}
.qw-notes{border-top:1px solid var(--qw-rule);padding-top:8px;margin-top:8px;font-size:.9em}
.qw-notes-wrap{background:var(--qw-page);box-shadow:var(--qw-page-shadow);padding:16px 24px;font-family:var(--qw-doc-font)}
.qw-notes-wrap:empty{display:none}
.qw-notes-wrap>.qw-notes:first-child{border-top:0;margin-top:0;padding-top:0}
.qw-notes-title{font:600 11px var(--qw-ui-font);color:var(--qw-ink-2);text-transform:uppercase;letter-spacing:.04em;margin-bottom:6px}
.qw-note{display:flex;gap:6px}.qw-note-label{min-width:1.2em;color:var(--qw-ink-2)}.qw-note-body{flex:1;min-width:0}
.qw-rail{position:relative;flex:0 0 var(--qw-rail-width);width:var(--qw-rail-width)}
.qw-bubble,.qw-card{position:absolute;left:0;right:0;background:var(--qw-bubble-bg);border:1px solid var(--qw-rule);
border-radius:var(--qw-radius);box-shadow:var(--qw-shadow);padding:8px 10px;transition:top .15s ease,box-shadow .15s}
.qw-bubble{border-inline-start:3px solid var(--qw-comment)}
.qw-card{border-inline-start:3px solid var(--qw-ins)}
.qw-bubble.qw-active{box-shadow:0 0 0 2px var(--qw-comment),var(--qw-shadow)}
.qw-bubble.qw-done{opacity:.7;border-inline-start-color:var(--qw-rule)}
.qw-bubble-head,.qw-card-head{display:flex;align-items:center;gap:6px;flex-wrap:wrap}
.qw-bubble-body{margin-top:4px;font-family:var(--qw-ui-font);font-size:var(--qw-ui-size)}
.qw-bubble-body .qw-p{margin:0 0 4px;min-height:0}
.qw-reply{border-top:1px solid var(--qw-rule);margin-top:6px;padding-top:6px}
.qw-avatar{display:inline-grid;place-items:center;width:20px;height:20px;border-radius:50%;font-size:10px;font-weight:600;
background:color-mix(in srgb,var(--qw-accent) 18%,transparent);color:var(--qw-accent)}
.qw-badge{font-size:10px;font-weight:600;padding:0 5px;border-radius:4px;background:color-mix(in srgb,var(--qw-accent) 15%,transparent);color:var(--qw-accent)}
.qw-meta{color:var(--qw-ink-2);font-size:11px}
.qw-actions{display:flex;gap:6px;margin-top:6px}
.qw-btn{font:inherit;font-size:12px;border-radius:6px;padding:2px 10px;cursor:pointer;border:1px solid var(--qw-rule);background:var(--qw-page);color:var(--qw-ink)}
.qw-btn-accept{border-color:var(--qw-ins);color:var(--qw-ins)}
.qw-btn-reject{border-color:var(--qw-del);color:var(--qw-del)}
.qw-btn:hover{background:color-mix(in srgb,currentColor 10%,var(--qw-page))}
.qw-pop{position:absolute;z-index:10;background:var(--qw-bubble-bg);border:1px solid var(--qw-rule);border-radius:var(--qw-radius);
box-shadow:var(--qw-shadow);padding:6px 8px;max-width:320px}
.qw-pop[hidden]{display:none}
.qw-pop-info{display:flex;flex-direction:column;gap:2px}
.qw-hl{background:var(--qw-hl);box-shadow:0 0 0 2px var(--qw-hl);border-radius:2px}
::highlight(qw-hl){background-color:rgba(255,200,0,.4)}
.qw-flash{animation:qw-flash var(--qw-flash-ms,1200ms) ease-out}
@keyframes qw-flash{from{background-color:var(--qw-flash);box-shadow:0 0 0 4px var(--qw-flash)}to{background-color:transparent;box-shadow:0 0 0 4px transparent}}
.qw-sr{position:absolute!important;width:1px;height:1px;overflow:hidden;clip:rect(0 0 0 0);white-space:nowrap}
@container (max-width:760px){
.qw-main{flex-direction:column;align-items:stretch}
.qw-rail{flex:none;width:auto;display:flex;flex-direction:column;gap:8px;min-height:0!important}
.qw-bubble,.qw-card{position:static}
.qw-sec{width:auto!important;min-height:0!important;padding:24px 16px!important}
.qw-table{width:100%!important}}
@media (prefers-reduced-motion:reduce){.qw-flash{animation:none;outline:2px solid var(--qw-flash)}.qw-bubble,.qw-card{transition:none}}
@media print{.qw-root{background:none;padding:0}.qw-rail,.qw-bar,.qw-pop{display:none}.qw-sec{box-shadow:none}}
`;
