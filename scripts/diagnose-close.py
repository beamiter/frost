import sys, time, json, importlib.util
from pathlib import Path
from types import SimpleNamespace
import native_scale_geometry
spec=importlib.util.spec_from_file_location('recovery',Path(__file__).with_name('native-recovery-report.py'))
r=importlib.util.module_from_spec(spec);spec.loader.exec_module(r)
spec=importlib.util.spec_from_file_location('smoke',Path(__file__).with_name('native-desktop-smoke.py'))
m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m)
for n in range(8):
    output=Path(sys.argv[3])/str(n);output.mkdir(parents=True)
    args=SimpleNamespace(binary=Path(sys.argv[1]).resolve(),pointer=Path(sys.argv[2]).resolve(),backend='wayland',output=output)
    result={};case=r.Case(m,args,'excess_sessions',result,time.monotonic()+240)
    try:
        case.execute()
    except BaseException as error:
        result.update(status='fail',error=repr(error))
        if case.app and case.app.poll() is None:
            result['thread_waits']={p.name:(p/'wchan').read_text() for p in Path(f'/proc/{case.app.pid}/task').iterdir()}
        case.smoke.screenshot('failure-before-cleanup')
        raise
    finally:
        case.cleanup();(output/'result.json').write_text(json.dumps(result,indent=2))
