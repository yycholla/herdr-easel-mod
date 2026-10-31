#!/usr/bin/env python3
"""Capture preview.png from a private headless Herdr, with synthetic review data."""
import json
import os
from pathlib import Path
import socket
import subprocess
import tempfile
import time

ROOT=Path(__file__).resolve().parents[1]
APP=Path(os.environ['HERDR_ORIGIN_WORKTREE'])
BIN=os.environ['HERDR_ORIGIN_BIN']
NAME=f'easel-preview-{os.getpid()}'
HOME=Path('/var/tmp')/f'herdr-app-{NAME}'/'home'
HELPER=ROOT/'target/debug/gpui-artifacts-mod'

with tempfile.TemporaryDirectory(prefix='easel-review-demo-') as temporary:
    project=Path(temporary)
    endpoint=project/'artifacts.sock'
    env=dict(os.environ,HERDR_APP_SANDBOX=NAME,HERDR_APP_BIN=BIN,HERDR_APP_HOME=str(HOME),GPUI_ARTIFACTS_SOCKET=str(endpoint))
    env.pop('GPUI_ARTIFACTS_VIEWER',None)
    HOME.mkdir(parents=True)
    (HOME/'shared.json').write_text('{"frame.welcomed":"skipped"}')
    target=HOME/'mods/easel';target.mkdir(parents=True)
    import shutil
    for name in ['main.js','renderer.js','mod.json']:shutil.copy2(ROOT/'mods/easel'/name,target/name)
    def sandbox(*args,check=True):
        result=subprocess.run([str(APP/'scripts/sandbox'),*args],env=env,capture_output=True,text=True,timeout=120)
        if check and result.returncode:raise RuntimeError(result.stdout+'\n'+result.stderr)
        return result.stdout
    def request(value):
        with socket.socket(socket.AF_UNIX) as conn:
            conn.connect(str(endpoint));conn.sendall((json.dumps(value)+'\n').encode());return json.loads(conn.makefile().readline())
    def action(value):
        return json.loads(subprocess.check_output([str(HELPER),'call',temporary,json.dumps(value)],env=env,text=True))
    backend=subprocess.Popen([str(HELPER),'serve',temporary],env=env,stdout=subprocess.DEVNULL,stderr=subprocess.PIPE)
    try:
        for _ in range(100):
            if endpoint.exists():break
            if backend.poll() is not None:raise RuntimeError(backend.stderr.read().decode())
            time.sleep(.025)
        source='''div().w(px(650.)).flex().flex_col().gap_4().p_6().rounded_xl()
    .bg(rgb(0x202132)).text_color(rgb(0xe6e5ef))
    .child(div().text_sm().font_weight(FontWeight::SEMIBOLD).text_color(rgb(0xb9a2ff)).child("DESIGN REVIEW"))
    .child(div().text_2xl().font_weight(FontWeight::BOLD).child("Make the next version clearer."))
    .child(div().text_color(rgb(0xb2b1c5)).child("A live GPUI preview, right beside the conversation."))
    .child(div().flex().gap_3()
        .child(div().flex_1().p_4().rounded_lg().bg(rgb(0x2b2c42))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Keep the context"))
            .child(div().mt_2().text_sm().text_color(rgb(0xb2b1c5)).child("Versions stay together.")))
        .child(div().flex_1().p_4().rounded_lg().bg(rgb(0x2b2c42))
            .child(div().font_weight(FontWeight::SEMIBOLD).child("Point to the change"))
            .child(div().mt_2().text_sm().text_color(rgb(0xb2b1c5)).child("Notes stay with their frame."))))
    .child(div().px_4().py_3().rounded_md().bg(rgb(0x7c3aed)).text_color(rgb(0xffffff))
        .child("Rendered natively. Reviewed together."))'''
        for version in [1,2]:
            text=source.replace('Make the next version clearer.','Review the first version.') if version==1 else source
            result=request({'type':'upsert','artifact':{'id':'review','title':'Review card','source':text,'mode':'interpret','frame':{'width':None,'height':None}}})
            assert result['type']=='artifact' and result['summary']['status']['state']=='rendered',result
        result=request({'type':'upsert','artifact':{'id':'preferences','title':'Preferences','source':(ROOT/'fixtures/preferences.rs').read_text(),'mode':'interpret','frame':{'width':None,'height':None}}})
        assert result['type']=='artifact',result
        action({'action':'select','id':'review','version':2})
        action({'action':'annotate','id':'review','version':2,'path':[1],'note':'More breathing room below the title.'})
        action({'action':'send','text':'Keep the purple accent. Make the heading easier to scan.'})
        assert request({'type':'say','text':'I’ll keep the accent and refine the next version.'})['type']=='done'
        sandbox('up')
        sandbox('do','space.new',json.dumps({'name':'Easel showcase','folder':temporary}))
        sandbox('do','tab.new',json.dumps({'name':'Easel','mod':'easel','props':{'project':temporary,'helper':str(HELPER)}}))
        for _ in range(80):
            if action({'action':'ping'}).get('attached'):break
            time.sleep(.05)
        else:raise RuntimeError('Easel mod did not attach')
        time.sleep(.5)
        status=json.loads(sandbox('status'));assert not status['errors'],status
        sandbox('shot',str(ROOT/'preview.png'))
        assert subprocess.check_output(['git','-C',str(APP),'status','--porcelain'],text=True)=='', 'Stock app worktree changed'
        print('Captured',ROOT/'preview.png','from stock Herdr, with synthetic project content only.')
    finally:
        sandbox('down',check=False)
        backend.terminate()
        try:backend.wait(timeout=5)
        except subprocess.TimeoutExpired:backend.kill();backend.wait()
        backend.stderr.close()
