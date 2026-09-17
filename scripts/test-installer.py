#!/usr/bin/env python3
"""Exercise the shipped shell installer in disposable homes, with no live installs."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPT = Path(__file__).resolve().with_name('install.sh')


class InstallerTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory(prefix='qbz-installer-test-')
        self.root = Path(self.tmp.name)
        self.home = self.root / 'home space $cash 100% "quoted" `ticks`'
        self.home.mkdir()
        self.bin = self.root / 'bin'
        self.bin.mkdir()
        for cmd in ('bash','id','python3','mktemp','rm','rmdir','grep','cat','tr',
                    'mkdir','cp','chmod','mv','od','wc','sha256sum','getconf'):
            (self.bin / cmd).symlink_to(shutil.which(cmd))
        (self.bin/'uname').write_text('#!/usr/bin/env bash\nif [ "$1" = -s ]; then printf "%s\\n" "${FIXTURE_OS:-Linux}"; else printf "%s\\n" "${FIXTURE_ARCH:-x86_64}"; fi\n')
        (self.bin/'uname').chmod(0o755)
        self.payload = b'\x7fELF' + b'\x00'*4 + b'AI\x02' + b'fixture only, never executed'
        (self.root/'package').write_bytes(self.payload)
        self.asset = {
            'name':'QBZ_2.1.2_amd64.AppImage',
            'browser_download_url':'https://github.com/vicrodh/qbz/releases/download/v2.1.2/QBZ_2.1.2_amd64.AppImage',
            'digest':'sha256:'+hashlib.sha256(self.payload).hexdigest(),
            'size':len(self.payload),
        }
        self.release = {'tag_name':'v2.1.2','draft':False,'prerelease':False,'assets':[self.asset]}
        # Only curl is replaced. Real bash, JSON parser, hashes and filesystem
        # operations exercise the production script, including spaces/$/% in HOME.
        (self.bin/'curl').write_text('#!'+sys.executable+'''
import os,sys,pathlib,shutil
args=sys.argv[1:]; root=pathlib.Path(os.environ['INSTALLER_FIXTURE'])
out=pathlib.Path(args[args.index('--output')+1]); url=args[-1]
with (root/'requests').open('a') as f: f.write(url+'\\n')
if url.startswith('https://api.github.com/repos/vicrodh/qbz/releases/'):
    shutil.copyfile(root/'release.json',out)
elif url=='https://github.com/vicrodh/qbz/releases/download/v2.1.2/QBZ_2.1.2_amd64.AppImage':
    shutil.copyfile(root/'package',out)
elif url=='https://qbz.lol/assets/brand/64x64.png': out.write_bytes(b'icon fixture')
else: raise SystemExit('Unexpected URL: '+url)
''')
        (self.bin/'curl').chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.home), PATH=str(self.bin),
                        TMPDIR=str(self.root), INSTALLER_FIXTURE=str(self.root))
        self.env.pop('BASH_ENV', None)
        self.dest = self.home/'.local/opt/qbz/QBZ.AppImage'

    def tearDown(self):
        self.tmp.cleanup()

    def run_installer(self,*args):
        (self.root/'release.json').write_text(json.dumps(self.release))
        return subprocess.run([shutil.which('bash'),str(SCRIPT),*args],env=self.env,text=True,capture_output=True,timeout=20)

    def test_install_verify_launchers_repeat_and_uninstall_keep_data(self):
        data=self.home/'.local/share/qbz/personal.db'
        data.parent.mkdir(parents=True); data.write_text('keep')
        r=self.run_installer(); self.assertEqual(r.returncode,0,r.stderr)
        self.assertEqual(self.dest.read_bytes(),self.payload)
        launcher=self.home/'.local/bin/qbz'
        self.assertIn('APPIMAGE_EXTRACT_AND_RUN=1',launcher.read_text())
        subprocess.run(['bash','-n',str(launcher)],check=True)
        self.assertIn('100%%',(self.home/'.local/share/applications/com.blitzfc.qbz.desktop').read_text())
        if shutil.which('desktop-file-validate'):
            subprocess.run(['desktop-file-validate',str(self.home/'.local/share/applications/com.blitzfc.qbz.desktop')],check=True)
        self.assertFalse(list(self.root.glob('qbz-install.*')))
        requests=(self.root/'requests').read_text()
        r=self.run_installer(); self.assertEqual(r.returncode,0,r.stderr)
        self.assertEqual((self.root/'requests').read_text(),requests)
        r=self.run_installer('--uninstall'); self.assertEqual(r.returncode,0,r.stderr)
        self.assertFalse(self.dest.exists()); self.assertFalse(launcher.exists())
        self.assertEqual(data.read_text(),'keep')

    def test_hash_failure_never_creates_installation(self):
        self.asset['digest']='sha256:'+'0'*64
        r=self.run_installer(); self.assertNotEqual(r.returncode,0)
        self.assertIn('checksum mismatch',r.stderr)
        self.assertFalse(self.dest.exists())

    def test_existing_app_and_launcher_are_preserved(self):
        self.dest.parent.mkdir(parents=True); self.dest.write_text('existing')
        self.assertNotEqual(self.run_installer().returncode,0)
        self.assertEqual(self.dest.read_text(),'existing')
        self.dest.unlink()
        launcher=self.home/'.local/bin/qbz'; launcher.parent.mkdir(parents=True)
        launcher.write_text('existing launcher')
        self.assertNotEqual(self.run_installer().returncode,0)
        self.assertEqual(launcher.read_text(),'existing launcher')

    def test_symlink_installation_is_not_followed(self):
        outside=self.root/'outside'; outside.mkdir()
        self.dest.parent.parent.mkdir(parents=True)
        self.dest.parent.symlink_to(outside,target_is_directory=True)
        self.assertNotEqual(self.run_installer().returncode,0)
        self.assertEqual(list(outside.iterdir()),[])

    def test_check_is_read_only_and_rejects_missing_digest_or_foreign_url(self):
        r=self.run_installer('--check'); self.assertEqual(r.returncode,0,r.stderr)
        self.assertFalse(self.dest.parent.exists())
        self.asset['digest']=''
        self.assertNotEqual(self.run_installer('--check').returncode,0)
        self.asset['digest']='sha256:'+'0'*64
        self.asset['browser_download_url']='https://example.org/payload'
        self.assertNotEqual(self.run_installer('--check').returncode,0)

    def test_older_release_and_prerelease_never_install(self):
        self.release['tag_name']='v2.1.1'
        r=self.run_installer(); self.assertNotEqual(r.returncode,0)
        self.assertIn('2.1.2 or newer',r.stderr)
        self.release['tag_name']='v2.1.2'; self.release['prerelease']=True
        self.assertNotEqual(self.run_installer().returncode,0)

    def test_all_four_platform_assets_are_selected(self):
        for system,arch,name in [('Linux','x86_64','QBZ_2.1.2_amd64.AppImage'),
                                 ('Linux','aarch64','QBZ_2.1.2_aarch64.AppImage'),
                                 ('Darwin','x86_64','QBZ_x64.app.tar.gz'),
                                 ('Darwin','arm64','QBZ_aarch64.app.tar.gz')]:
            with self.subTest(system=system,arch=arch):
                self.env.update(FIXTURE_OS=system,FIXTURE_ARCH=arch)
                self.asset['name']=name
                self.asset['browser_download_url']='https://github.com/vicrodh/qbz/releases/download/v2.1.2/'+name
                r=self.run_installer('--check')
                self.assertEqual(r.returncode,0,r.stderr)
                self.assertIn(name,r.stdout)

    def test_check_cannot_accidentally_uninstall(self):
        r=self.run_installer('--check','--uninstall')
        self.assertNotEqual(r.returncode,0)
        self.assertIn('cannot be combined',r.stderr)

    @unittest.skipUnless(shutil.which('jq'), 'jq is required for its parser fallback test')
    def test_jq_parser_works_without_python_on_path(self):
        (self.bin/'python3').unlink()
        (self.bin/'jq').symlink_to(shutil.which('jq'))
        r=self.run_installer()
        self.assertEqual(r.returncode,0,r.stderr)
        self.assertEqual(self.dest.read_bytes(),self.payload)


if __name__=='__main__': unittest.main()
