# Search benchmark for mini --move. Pass = query token(s) in url_after (not the model's ok).
import json, os, subprocess, sys, time, urllib.parse
HERE = os.path.dirname(os.path.abspath(__file__))
MINI = os.path.join(HERE, "..", "..", "target", "debug", "mini.exe")
tasks = json.load(open(os.path.join(HERE, "search.json")))
only = set(sys.argv[2:])
label = sys.argv[1] if len(sys.argv) > 1 else "run"
out_dir = os.path.join(os.environ["LOCALAPPDATA"], "Mini", "bench", label)
os.makedirs(out_dir, exist_ok=True)
rows = []
restarts = 0

def ensure_browser():
    global restarts
    if subprocess.run([MINI, "--debug-status"], capture_output=True).returncode == 0:
        return False
    restarts += 1
    subprocess.run([MINI, "--debug", "about:blank"], capture_output=True, timeout=60)
    time.sleep(3)
    return True

for t in tasks:
    if only and t["name"] not in only:
        continue
    goal = (f'Search this site for "{t["query"]}" using its search box, and submit the search. '
            f'Stop when the search results list for "{t["query"]}" is visible. Do not open a single result.')
    restarted = ensure_browser()
    t0 = time.time()
    p = subprocess.run([MINI, "--move", "--allow-remote", "--url", t["url"], "--goal", goal, "--steps", "15"],
                       capture_output=True, text=True, encoding="utf-8", errors="replace", timeout=240)
    try:
        d = json.loads(p.stdout.strip().splitlines()[-1])
    except Exception:
        d = {"ok": False, "stop": "no json: " + (p.stderr or "")[-300:], "steps": [], "url_after": "", "judge_calls": 0}
    url = urllib.parse.unquote_plus(d.get("url_after", "")).lower()
    passed = all(m in url for m in t["must"])
    row = dict(name=t["name"], passed=passed, restarted=restarted, model_ok=d.get("ok"), calls=d.get("judge_calls"),
               secs=round(time.time() - t0, 1), url_after=d.get("url_after"), stop=d.get("stop"),
               steps=[f'{s["operation"]}:{(s.get("target") or "")[:40]}{"" if s.get("page_changed") else "(no change)"}' + (f' [{s["note"]}]' if s.get("note") else "") for s in d.get("steps", [])])
    rows.append(row)
    json.dump(d, open(os.path.join(out_dir, t["name"] + ".json"), "w", encoding="utf-8"), ensure_ascii=False)
    print(json.dumps(row, ensure_ascii=False), flush=True)
json.dump(rows, open(os.path.join(out_dir, "summary.json"), "w", encoding="utf-8"), ensure_ascii=False, indent=1)
false_ok = sum(1 for r in rows if r["model_ok"] and not r["passed"])
print(f"PASS {sum(r['passed'] for r in rows)}/{len(rows)}  model_ok {sum(bool(r['model_ok']) for r in rows)}  false_ok {false_ok}  browser_restarts {restarts}")
