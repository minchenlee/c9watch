#!/usr/bin/env python3
"""Rebuild the subscription QA app; bound generated files without deleting source."""
import fcntl
import hashlib
import json
import os
from pathlib import Path
import plistlib
import shutil
import subprocess
import tarfile

ROOT = Path(__file__).resolve().parents[1]
OUTPUT = ROOT / '.qa-build'
TARGET = OUTPUT / 'target'
APP = Path.home() / 'Applications/c9watch Subscription QA.app'
STAGE = APP.with_name('.c9watch-subscription-qa.next.app')
IDENTIFIER = 'com.minchenlee.c9watch.subscription-qa'
OWNER = 'c9watch-subscription-qa-v1'


def remove_generated(path):
    if path.is_symlink():
        raise RuntimeError(f'Refusing symlink: {path}')
    if path == TARGET:
        if (OUTPUT / 'owner').read_text() != OWNER:
            raise RuntimeError('Build directory ownership mismatch')
    elif path in (APP, STAGE):
        info = plistlib.loads((path / 'Contents/Info.plist').read_bytes())
        if info.get('CFBundleIdentifier') != IDENTIFIER:
            raise RuntimeError(f'Unexpected app at {path}')
        binary = path / 'Contents/MacOS/c9watch'
        running = subprocess.run(['/usr/sbin/lsof', '-t', str(binary)], capture_output=True)
        if running.returncode == 0 and running.stdout.strip():
            raise RuntimeError(f'Quit {path.name} before replacing it')
    else:
        raise RuntimeError(f'Unmanaged cleanup path: {path}')
    if path.exists():
        shutil.rmtree(path)


def main():
    if OUTPUT.is_symlink():
        raise RuntimeError('Refusing symlinked build directory')
    if OUTPUT.exists() and not (OUTPUT / 'owner').is_file():
        raise RuntimeError('Build directory exists without ownership marker')
    OUTPUT.mkdir(exist_ok=True)
    if (OUTPUT / 'owner').exists() and (OUTPUT / 'owner').read_text() != OWNER:
        raise RuntimeError('Build directory ownership mismatch')
    (OUTPUT / 'owner').write_text(OWNER)
    with (OUTPUT / 'rebuild.lock').open('w') as lock, (OUTPUT / 'build.log').open('w') as log:
        fcntl.flock(lock, fcntl.LOCK_EX | fcntl.LOCK_NB)
        def run(args):
            print(' '.join(map(str, args)), flush=True)
            subprocess.run(list(map(str,args)), cwd=ROOT, stdout=log, stderr=subprocess.STDOUT, check=True)
        cargo = ['cargo', '--config', 'profile.qa.inherits="dev"', '--config', 'profile.qa.debug=0', '--config', 'profile.qa.incremental=false']
        common = ['--manifest-path', ROOT/'src-tauri/Cargo.toml', '--target-dir', TARGET, '--profile', 'qa']
        # One dependency cache. A changed lockfile/toolchain invalidates the old cache.
        fingerprint = hashlib.sha256((ROOT/'src-tauri/Cargo.lock').read_bytes() + subprocess.check_output(['rustc','--version']) + b'qa-debug0-incremental0-v1').hexdigest()
        stamp = OUTPUT/'dependency-fingerprint'
        if TARGET.exists() and (not stamp.exists() or stamp.read_text() != fingerprint):
            remove_generated(TARGET)
        stamp.write_text(fingerprint)
        if STAGE.exists():
            remove_generated(STAGE)
        run(['npm','run','check'])
        run(['npm','run','build'])
        for script in ['test-subagent-polling.mjs','test-subscription-polling.mjs','test-usage-preferences.mjs']:
            run(['node',ROOT/'scripts'/script])
        run(cargo + ['test'] + common + ['--features','tauri/custom-protocol','--lib','usage'])
        run(cargo + ['build'] + common + ['--features','tauri/custom-protocol','--bin','c9watch'])
        binary = TARGET/'qa/c9watch'
        run(['node',ROOT/'scripts/test-claude-usage-bridge.mjs',binary])
        version = json.loads((ROOT/'src-tauri/tauri.conf.json').read_text())['version']
        contents = STAGE/'Contents'
        contents.mkdir(parents=True)
        (contents/'Info.plist').write_bytes(plistlib.dumps({
            'CFBundleExecutable':'c9watch','CFBundleIdentifier':IDENTIFIER,
            'CFBundleName':'c9watch Subscription QA','CFBundlePackageType':'APPL',
            'CFBundleVersion':version,'CFBundleShortVersionString':version,
            'CFBundleIconFile':'icon.icns','NSHighResolutionCapable':True}))
        (contents/'MacOS').mkdir()
        (contents/'Resources').mkdir()
        shutil.copy2(binary,contents/'MacOS/c9watch')
        shutil.copy2(ROOT/'src-tauri/icons/icon.icns',contents/'Resources/icon.icns')
        # Existing verified source edits also survive in one replaceable recovery archive.
        changed = subprocess.check_output(['git','ls-files','--modified','--others','--exclude-standard','-z'],cwd=ROOT).decode().split('\0')
        with tarfile.open(OUTPUT/'source-snapshot.tar.gz','w:gz') as archive:
            for name in sorted(set(filter(None, changed))):
                path = ROOT/name
                if path.is_file() and not path.is_symlink():
                    archive.add(path,arcname=name,recursive=False)
        digest = hashlib.sha256((contents/'MacOS/c9watch').read_bytes()).hexdigest()
        if APP.exists():
            remove_generated(APP)
        STAGE.rename(APP)
        # Remove this package's old executables, test binaries and duplicate libraries.
        # Dependency artifacts remain reusable; incremental compilation is disabled.
        run(cargo + ['clean'] + common + ['--package','c9watch'])
        (OUTPUT/'latest-build.json').write_text(json.dumps({'app':str(APP),'sha256':digest,'source':str(ROOT),'base':subprocess.check_output(['git','rev-parse','HEAD'],cwd=ROOT).decode().strip()},indent=2)+'\n')
        print(f'Ready: {APP}\nSHA-256: {digest}\nSource backup: {OUTPUT}/source-snapshot.tar.gz',flush=True)

if __name__ == '__main__':
    main()
