#![cfg(unix)]
use ai_token_timeline::profiles::{self, Profile, Settings, TeamSpec};
use serde_json::{Value, json};
use std::{
    fs,
    os::unix::fs::PermissionsExt,
    path::Path,
    process::{Command, Output},
};

const ROOT: &str = r#"#!/usr/bin/env python3
import sys, json, subprocess, os, time
from pathlib import Path
args=sys.argv[1:]
prompt=sys.stdin.read()
Path('root.json').write_text(json.dumps({'args':args,'prompt':prompt}))
overrides={}
for i,arg in enumerate(args[:-1]):
    if arg=='-c':
        key,value=args[i+1].split('=',1); overrides[key]=value
if '--mcp-config' in args:
    config=json.loads(args[args.index('--mcp-config')+1])
    if not config['mcpServers']:
        config=json.loads(args[args.index('--mcp-config',args.index('--mcp-config')+1)+1])
    conf=config['mcpServers']['stackpulse_team']; exe=conf['command']; argv=conf['args']
else:
    assert overrides['features.multi_agent']=='false'
    exe=json.loads(overrides['mcp_servers.stackpulse_team.command'])
    argv=json.loads(overrides['mcp_servers.stackpulse_team.args'])
Path('bridge-path').write_text(argv[-1])
p=subprocess.Popen([exe]+argv,stdin=subprocess.PIPE,stdout=subprocess.PIPE,text=True)
def rpc(method,params={}):
    p.stdin.write(json.dumps({'jsonrpc':'2.0','id':1,'method':method,'params':params})+'\n'); p.stdin.flush()
    line=p.stdout.readline(); assert line, 'bridge closed unexpectedly'
    result=json.loads(line); assert 'result' in result, result
    return result['result']
def call(name,arguments):
    result=rpc('tools/call',{'name':name,'arguments':arguments})
    value=json.loads(result['content'][0]['text']); return result.get('isError'),value
assert rpc('initialize',{})['capabilities']=={'tools':{}}
assert len(rpc('tools/list')['tools'])==4
error,_=call('spawn',{'role':'unauthorized','prompt':'task'}); assert error
error,job=call('spawn',{'role':'worker','prompt':'Complete bounded child task.','timeout_secs':3 if os.environ.get('SCENARIO')=='child_timeout' else 30}); assert not error,job
id=job['id']
mode=os.environ.get('SCENARIO','success')
if mode in ['cancel','root_timeout','eof','bridge_crash']:
    while not Path('helper-pid').exists():time.sleep(.02)
    error,value=call('spawn',{'role':'worker','prompt':'must exceed concurrency'}); assert error,value
if mode=='exec_failure_parallel':
    while not Path('helper-pid').exists():time.sleep(.02)
    Path('grok').write_text('#!/missing-interpreter-stackpulse-fixture\n')
    error,failed=call('spawn',{'role':'worker','prompt':'must fail before exec'});assert not error,failed
    while True:
        error,failed=call('wait',{'id':failed['id'],'timeout_secs':1});assert not error,failed
        if failed['status']!='running':break
    assert failed['status']=='failed' and 'error' in failed,failed
    markers=list(Path(argv[-1]).parent.glob('pid-*'))
    assert len(markers)==1 and id in markers[0].name,markers
if mode=='root_timeout':
    time.sleep(30)
if mode=='bridge_crash':
    p.kill();p.wait(timeout=5);sys.exit(7)
if mode=='eof':
    p.stdin.close();p.wait(timeout=5); result={'status':'cancelled-by-eof'}
else:
    if mode in ['cancel','exec_failure_parallel']: call('cancel',{'id':id})
    while True:
        error,result=call('wait',{'id':id,'timeout_secs':1});assert not error,result
        if result['status']!='running':break
    assert not list(Path(argv[-1]).parent.glob('pid-*')), 'terminal task retained a PID marker'
    p.stdin.close();p.wait(timeout=5)
Path('result.json').write_text(json.dumps(result))
if '--print' in args:
    print(json.dumps({'type':'result','subtype':'success','is_error':False,'result':json.dumps(result)}))
else:
    print(json.dumps({'type':'thread.started','thread_id':'mixed-fixture'}))
    print(json.dumps({'type':'item.completed','item':{'type':'agent_message','text':json.dumps(result)}}))
    print(json.dumps({'type':'turn.completed','usage':{'input_tokens':3,'output_tokens':2}}))
"#;
const CHILD: &str = r#"#!/usr/bin/env python3
import sys,json,os,time,signal,subprocess
from pathlib import Path
args=sys.argv[1:];prompt=sys.stdin.read()
Path('child.json').write_text(json.dumps({'args':args,'prompt':prompt,'pid':os.getpid()}))
mode=os.environ.get('SCENARIO','success')
if mode in ['cancel','root_timeout','eof','child_timeout','bridge_crash','exec_failure_parallel']:
    signal.signal(signal.SIGINT,signal.SIG_IGN)
    helper=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'])
    Path('helper-pid').write_text(str(helper.pid))
    time.sleep(60)
if mode=='background_helper':
    helper=subprocess.Popen([sys.executable,'-c','import time; time.sleep(60)'],stdin=subprocess.DEVNULL,stdout=subprocess.DEVNULL,stderr=subprocess.DEVNULL)
    Path('helper-pid').write_text(str(helper.pid))
if mode=='failure':
    print(json.dumps({'type':'error','message':'fixture provider unavailable'}));sys.exit(9)
print(json.dumps({'type':'end','stopReason':'end_turn','text':'verified Grok child result','usage':{'input_tokens':5,'output_tokens':2,'cache_read_input_tokens':0,'cache_creation_input_tokens':0}}))
"#;
fn executable(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}
fn run(root: &Path, scenario: &str, claude: bool) -> Output {
    let profiles_dir = root.join("providers/project/all");
    fs::create_dir_all(&profiles_dir).unwrap();
    let team:TeamSpec=serde_json::from_value(json!({"name":"mixed","provider":if claude{"anthropic"}else{"openai"},"orchestrator":{"role":"root","model":if claude{"claude-root"}else{"gpt-6-astra"},"effort":"medium","purpose":"Coordinate","when":"Always"},"agents":[{"provider":"xai","role":"worker","model":"grok-4.3","effort":"high","purpose":"Implement bounded task","when":"As needed"}],"delegation":if scenario=="exec_failure_parallel"{"on_demand"}else{"sequential"},"integration":"Verify child results","notes":"fixture"})).unwrap();
    let profile = Profile {
        schema_version: 1,
        source_image: "mixed.png".into(),
        source_sha256: profiles::digest(b"fixture"),
        generated_by: "fixture".into(),
        generated_at: chrono::Utc::now(),
        team,
    };
    fs::write(profiles_dir.join("mixed.png"), b"fixture").unwrap();
    fs::write(
        profiles_dir.join("mixed.md"),
        profiles::markdown(&profile).unwrap(),
    )
    .unwrap();
    let backend = if claude {
        ai_token_timeline::client::Backend::Claude
    } else {
        ai_token_timeline::client::Backend::Codex
    };
    let settings = Settings {
        schema_version: 1,
        client: backend,
        provider: backend.default_provider().into(),
        model: "default".into(),
        effort: "medium".into(),
        executable: root.join(backend.command()),
        providers_root: root.join("providers"),
        default_profile: Some("mixed".into()),
        max_agents: 3,
        ai_memory: false,
        ai_usagebar: false,
    };
    executable(&settings.executable, ROOT);
    executable(
        &root.join("grok"),
        if scenario == "exec_failure" {
            "#!/missing-interpreter-stackpulse-fixture\n"
        } else {
            CHILD
        },
    );
    settings.save(&root.join("config.json")).unwrap();
    Command::new(env!("CARGO_BIN_EXE_ai-token-timeline"))
        .current_dir(root)
        .args([
            "--allow-workspace",
            root.to_str().unwrap(),
            "--config",
            "config.json",
            "--db",
            "usage.sqlite",
            "--sessions",
            "sessions",
            "run",
            "delegate a bounded task",
            "--profile",
            "mixed",
            "--no-feedback",
            "--sandbox",
            "read-only",
            "--timeout",
            if scenario == "root_timeout" {
                "4"
            } else {
                "10"
            },
        ])
        .env(
            "PATH",
            format!(
                "{}:{}",
                root.display(),
                std::env::var("PATH").unwrap_or_default()
            ),
        )
        .args(if scenario == "no_policy" {
            vec!["--no-policy"]
        } else {
            vec![]
        })
        .env("SCENARIO", scenario)
        .output()
        .unwrap()
}
fn value(root: &Path, name: &str) -> Value {
    serde_json::from_slice(&fs::read(root.join(name)).unwrap()).unwrap()
}
fn gone(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) != 0 }
}
fn assert_cleanup(root: &Path) {
    let config = fs::read_to_string(root.join("bridge-path")).unwrap();
    assert!(
        !Path::new(&config).exists(),
        "private bridge config must be removed"
    );
    let mut pids = vec![value(root, "child.json")["pid"].as_i64().unwrap() as i32];
    if let Ok(helper) = fs::read_to_string(root.join("helper-pid")) {
        pids.push(helper.parse().unwrap());
    }
    for _ in 0..100 {
        if pids.iter().all(|pid| gone(*pid)) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    panic!("child/helper {pids:?} survived root exit");
}
#[test]
fn codex_root_dispatches_real_mcp_to_grok_and_returns_result() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = run(root, "success", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let child = value(root, "child.json");
    let args = child["args"].as_array().unwrap();
    for value in [
        "grok-4.3",
        "--reasoning-effort",
        "high",
        "--no-subagents",
        "--permission-mode",
        "plan",
    ] {
        assert!(args.iter().any(|a| a == value), "{args:?}");
    }
    assert!(
        child["prompt"]
            .as_str()
            .unwrap()
            .contains("Complete bounded child task")
    );
    assert_eq!(
        value(root, "result.json")["outcome"]["final_message"],
        "verified Grok child result"
    );
    assert_cleanup(root);
}
#[test]
fn claude_root_registers_same_bridge() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(temp.path(), "success", true);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(value(temp.path(), "result.json")["status"], "completed");
    assert_cleanup(temp.path());
}
#[test]
fn child_failure_is_returned_as_failure_without_model_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(temp.path(), "failure", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = value(temp.path(), "result.json");
    assert_eq!(result["status"], "failed");
    assert_eq!(result["outcome"]["exit_code"], 9);
    assert_eq!(
        result["outcome"]["error_message"],
        "fixture provider unavailable"
    );
    assert_cleanup(temp.path());
}
#[test]
fn cancel_and_stdio_eof_kill_running_children() {
    for scenario in ["cancel", "eof"] {
        let temp = tempfile::tempdir().unwrap();
        let output = run(temp.path(), scenario, false);
        assert!(
            output.status.success(),
            "{scenario}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        if scenario == "cancel" {
            assert_eq!(
                value(temp.path(), "result.json")["outcome"]["interrupted"],
                true
            );
        }
        assert_cleanup(temp.path());
    }
}
#[test]
fn root_timeout_kills_child_group_even_if_bridge_is_killed() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(temp.path(), "root_timeout", false);
    assert!(!output.status.success());
    assert!(
        temp.path().join("child.json").exists(),
        "stdout={} stderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_cleanup(temp.path());
}

#[test]
fn child_timeout_returns_terminal_failure_to_root() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(temp.path(), "child_timeout", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(
        value(temp.path(), "result.json")["outcome"]["timed_out"],
        true
    );
    assert_cleanup(temp.path());
}

#[test]
fn abrupt_bridge_crash_does_not_orphan_children() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(temp.path(), "bridge_crash", false);
    assert!(!output.status.success());
    assert!(
        temp.path().join("helper-pid").exists(),
        "fixture helper was started"
    );
    assert_cleanup(temp.path());
}

#[test]
fn completed_child_does_not_leave_background_helpers() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(temp.path(), "background_helper", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(value(temp.path(), "result.json")["status"], "completed");
    assert!(temp.path().join("helper-pid").exists());
    assert_cleanup(temp.path());
}

#[test]
fn exec_failure_removes_its_pid_registration_before_returning_error() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(temp.path(), "exec_failure", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let result = value(temp.path(), "result.json");
    assert_eq!(result["status"], "failed");
    assert!(
        result["error"]
            .as_str()
            .unwrap()
            .contains("Não foi possível iniciar")
    );
    let config = fs::read_to_string(temp.path().join("bridge-path")).unwrap();
    assert!(!Path::new(&config).exists());
}

#[test]
fn exec_failure_preserves_registration_of_another_running_task() {
    let temp = tempfile::tempdir().unwrap();
    let output = run(temp.path(), "exec_failure_parallel", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert_cleanup(temp.path());
}

#[test]
fn no_policy_flows_from_cli_through_bridge_to_grok() {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path();
    let output = run(root, "no_policy", false);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let parent = value(root, "root.json");
    let args = parent["args"].as_array().unwrap();
    assert!(args.iter().any(|a| a == "approval_policy=\"never\""));
    assert!(args.iter().any(|a| a == "danger-full-access"));
    let child = value(root, "child.json");
    let args = child["args"].as_array().unwrap();
    assert!(
        args.windows(2)
            .any(|p| p[0] == "--permission-mode" && p[1] == "bypassPermissions")
    );
    assert!(
        args.windows(2)
            .any(|p| p[0] == "--sandbox" && p[1] == "off")
    );
    assert_cleanup(root);
}
