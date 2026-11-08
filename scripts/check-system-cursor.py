#!/usr/bin/env python3
"""Check our GPUI harness on an isolated X11 display or nested Weston compositor.

Run through xvfb-run. This opens only its own test windows and never connects
an RDP session or reads credentials. See --help and the root README.
"""
import argparse, ctypes, json, os, re, shutil, subprocess, tempfile, time
from pathlib import Path
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--backend', choices=['x11', 'wayland'], default='x11')
parser.add_argument('--test-binary', type=Path)
parser.add_argument('--weston-root', type=Path)
parser.add_argument('--scale', type=int, choices=[1, 2], default=1)
parser.add_argument('--software-vulkan', action='store_true')
args = parser.parse_args()
mode = args.backend
root = Path(__file__).resolve().parent.parent
os.chdir(root)
(root / '.interop').mkdir(exist_ok=True)
if args.test_binary:
    exe = args.test_binary.resolve(strict=True)
else:
    build = subprocess.run(['cargo', 'test', '--locked', '--bin', 'rumstsc', '--no-run', '--message-format=json'], text=True, stdout=subprocess.PIPE, check=True)
    artifacts = [json.loads(line) for line in build.stdout.splitlines() if line.startswith('{')]
    exe = Path(next((item['executable'] for item in reversed(artifacts) if item.get('reason') == 'compiler-artifact' and item.get('target', {}).get('name') == 'rumstsc' and item.get('executable'))))
env = os.environ.copy()
if args.software_vulkan:
    driver = Path('/usr/share/vulkan/icd.d/lvp_icd.json')
    if not driver.is_file():
        parser.error('Mesa lavapipe ICD is not installed')
    env['VK_DRIVER_FILES'] = str(driver)
processes = []
log = open(f'.interop/system-cursor-{mode}.log', 'w')

def xdo(*args):
    return subprocess.check_output(['xdotool', *map(str, args)], text=True, stderr=subprocess.DEVNULL).strip()

def wait_window(name):
    for _ in range(150):
        try:
            ids = xdo('search', '--name', name)
            if ids:
                return ids.splitlines()[-1]
        except subprocess.CalledProcessError:
            pass
        time.sleep(0.02)
    raise RuntimeError('test window not mapped: ' + name)

def cursor_image():

    class Image(ctypes.Structure):
        _fields_ = [('x', ctypes.c_short), ('y', ctypes.c_short), ('width', ctypes.c_ushort), ('height', ctypes.c_ushort), ('hot_x', ctypes.c_ushort), ('hot_y', ctypes.c_ushort), ('serial', ctypes.c_ulong), ('pixels', ctypes.POINTER(ctypes.c_ulong)), ('atom', ctypes.c_ulong), ('name', ctypes.c_char_p)]
    x11 = ctypes.CDLL('libX11.so.6')
    fixes = ctypes.CDLL('libXfixes.so.3')
    x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
    x11.XOpenDisplay.restype = ctypes.c_void_p
    x11.XCloseDisplay.argtypes = [ctypes.c_void_p]
    x11.XFree.argtypes = [ctypes.c_void_p]
    fixes.XFixesGetCursorImage.argtypes = [ctypes.c_void_p]
    fixes.XFixesGetCursorImage.restype = ctypes.POINTER(Image)
    display = x11.XOpenDisplay(None)
    assert display
    pointer = fixes.XFixesGetCursorImage(display)
    assert pointer
    image = pointer.contents
    result = {'width': image.width, 'height': image.height, 'hot_x': image.hot_x, 'hot_y': image.hot_y, 'pixels': [image.pixels[i] & 4294967295 for i in range(image.width * image.height)]}
    x11.XFree(pointer)
    x11.XCloseDisplay(display)
    return result
try:
    with tempfile.TemporaryDirectory(prefix='rumstsc-wayland-') as runtime:
        if mode == 'wayland':
            if args.weston_root:
                base = args.weston_root.resolve() / 'usr'
                libs = base / 'lib/x86_64-linux-gnu'
                env['LD_LIBRARY_PATH'] = str(libs) + ':' + str(libs / 'weston')
                env['WESTON_MODULE_MAP'] = ';'.join((str(p.name) + '=' + str(p) for p in libs.rglob('*.so')))
                weston_exe = str(base / 'bin/weston')
                shell = str(libs / 'weston/kiosk-shell.so')
            else:
                weston_exe = shutil.which('weston')
                if not weston_exe:
                    parser.error('Install Weston or pass --weston-root')
                shell = 'kiosk-shell.so'
            env['XDG_RUNTIME_DIR'] = runtime
            env['WAYLAND_DISPLAY'] = 'rumstsc-cursor-test'
            weston_log = open('.interop/system-cursor-weston.log', 'w')
            weston = subprocess.Popen([weston_exe, '--backend=x11', '--shell=' + shell, '--use-pixman', '--socket=rumstsc-cursor-test', '--idle-time=0', '--width=800', '--height=600', '--scale=' + str(args.scale)], env=env, stdout=weston_log, stderr=subprocess.STDOUT)
            processes.append(weston)
            for _ in range(150):
                if Path(runtime, 'rumstsc-cursor-test').exists():
                    break
                if weston.poll() is not None:
                    raise RuntimeError('Weston exited; see compositor log')
                time.sleep(0.02)
            else:
                raise RuntimeError('Wayland socket missing')
            window = re.findall('window id (\\d+)', Path('.interop/system-cursor-weston.log').read_text())[-1]
            xdo('windowmap', '--sync', window)
            time.sleep(0.1)
            xdo('windowfocus', window)
            env['WAYLAND_DEBUG'] = '1'
        else:
            env.pop('WAYLAND_DISPLAY', None)
        app = subprocess.Popen([str(exe), 'cursor_redraw_reuses', '--ignored', '--nocapture', '--test-threads=1'], env=env, stdout=log, stderr=subprocess.STDOUT)
        processes.append(app)
        if mode == 'x11':
            window = wait_window('^rumstsc render regression$')
        xdo('windowmap', '--sync', window)
        time.sleep(0.08)
        xdo('windowfocus', window)
        xdo('mousemove', '--window', window, 50, 50)
        time.sleep(0.1)
        if mode == 'x11':
            image = cursor_image()
            scale = image['width'] // 16
            assert scale > 0 and image['width'] == 16 * scale and (image['height'] == 16 * scale), image
            assert (image['hot_x'], image['hot_y']) == (3 * scale, 4 * scale), image
            assert all((pixel == (2150110218 if x < scale and y < scale else 4294902015) for y in range(image['height']) for x, pixel in enumerate(image['pixels'][y * image['width']:(y + 1) * image['width']]))), image
            print(json.dumps({k: v for k, v in image.items() if k != 'pixels'}))
            xdo('mousemove', '--window', window, 350, 210)
            time.sleep(0.08)
            outside = cursor_image()
            assert outside != image, 'local cursor was not restored'
            xdo('mousemove', '--window', window, 50, 50)
            time.sleep(0.08)
            assert cursor_image()['pixels'][0] == 2150110218
        if mode == 'wayland':
            # Mapping/configuration is asynchronous: do not test leaving the
            # desktop until the compositor has selected its first bitmap.
            for _ in range(150):
                if ', 3, 4)' in Path('.interop/system-cursor-wayland.log').read_text():
                    break
                if app.poll() is not None:
                    raise RuntimeError('test exited before cursor selection')
                time.sleep(.02)
            else:
                raise RuntimeError('custom cursor was never selected')
            xdo('mousemove', '--window', window, 350 * args.scale, 210 * args.scale)
            time.sleep(0.08)
            xdo('mousemove', '--window', window, 50, 50)
            time.sleep(0.08)
        for i in range(20):
            xdo('mousemove', '--window', window, 50 + i, 60)
            time.sleep(0.005)
        if mode == 'x11':
            time.sleep(0.7)
            replacement = cursor_image()
            assert (replacement['width'], replacement['height'], replacement['hot_x'], replacement['hot_y']) == (24 * scale, 24 * scale, 5 * scale, 6 * scale), replacement
            assert all((pixel == 4280207370 for pixel in replacement['pixels'])), replacement
        result = app.wait(timeout=30)
        assert result == 0, 'GPUI test failed; see log'
        log.flush()
        if mode == 'wayland':
            trace = Path('.interop/system-cursor-wayland.log').read_text()
            assert 'set_cursor' in trace and ', 3, 4)' in trace, 'custom cursor surface was not selected'
            assert re.search('create_buffer\\(.*' + str(16 * args.scale) + ', ' + str(16 * args.scale) + ', ' + str(64 * args.scale), trace), 'custom ARGB buffer missing'
            assert ', 5, 6)' in trace, 'replacement hotspot missing'
            cursor_requests = [line.strip() for line in trace.splitlines() if ' -> ' in line and ('set_cursor(' in line or '.set_shape(' in line)]
            custom_requests = [i for i, line in enumerate(cursor_requests) if line.endswith(', 3, 4)')]
            assert len(custom_requests) >= 2, 'custom cursor was not restored on re-entry'
            assert any((not line.endswith((', 3, 4)', ', 5, 6)')) for line in cursor_requests[custom_requests[0] + 1:custom_requests[-1]])), 'local cursor was not restored outside the desktop'
            assert 'set_buffer_scale(' + str(args.scale) + ')' in trace, 'DPI scale missing'
        print(mode + ': native cursor and GPU regression passed')
finally:
    for process in reversed(processes):
        if process.poll() is None:
            process.terminate()
            try:
                process.wait(timeout=5)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
    log.close()
