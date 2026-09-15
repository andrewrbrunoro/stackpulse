#!/usr/bin/env python3
"""Offline Codex stdio fixture. Never invokes a provider or reads user configuration.

Optional fake-app-server.json in cwd: lifetime_seconds (default 30), mode
(success, reject, unconfirmed, malformed_ack, foreign), finish_after_control,
record_file. The root remains active so terminal QA can send several interventions.
"""
import json
import os
import re
import signal
import sys
import threading
import time
from pathlib import Path

ROOT = "00000000-0000-4000-8000-000000000001"
CHILD = "00000000-0000-4000-8000-000000000002"
TURN = "00000000-0000-4000-8000-000000000003"
settings_file = Path("fake-app-server.json")
config = json.loads(settings_file.read_text()) if settings_file.exists() else {}
signal.signal(signal.SIGINT, lambda *_: sys.exit(0))
mode = config.get("mode", "success")
lock = threading.Lock()
finished = threading.Event()


def emit(value):
    with lock:
        print(json.dumps(value), flush=True)


def event(method, params):
    emit({"method": method, "params": params})


def reply(request, result):
    emit({"id": request["id"], "result": result})


def complete():
    if finished.is_set():
        return
    finished.set()
    event("item/completed", {"threadId": CHILD, "item": {"type": "agentMessage", "id": "child-final", "text": "Verificação concluída.", "phase": "final_answer"}})
    event("turn/completed", {"threadId": CHILD, "turn": {"id": "child-turn", "status": "completed"}})
    event("item/completed", {"threadId": ROOT, "item": {"type": "agentMessage", "id": "root-final", "text": "Trabalho de demonstração concluído.", "phase": "final_answer"}})
    event("turn/completed", {"threadId": ROOT, "turn": {"id": TURN, "status": "completed"}})


def delayed_complete():
    if not finished.wait(float(config.get("lifetime_seconds", 30))):
        complete()


def native_call(request_id, name, args, kind):
    call_id = f"call-{request_id}-{name}"
    event("rawResponseItem/completed", {"threadId": ROOT, "turnId": TURN, "item": {
        "type": "function_call", "namespace": "collaboration", "name": name,
        "arguments": json.dumps(args), "call_id": call_id}})
    event("item/completed", {"threadId": ROOT, "turnId": TURN, "item": {
        "type": "subAgentActivity", "id": call_id, "kind": kind,
        "agentThreadId": args["target"], "agentPath": "/root/worker"}})


for line in sys.stdin:
    request = json.loads(line)
    if config.get("record_file"):
        with open(config["record_file"], "a") as record:
            record.write(json.dumps(request) + "\n")
    method = request.get("method")
    params = request.get("params", {})
    if method == "initialize":
        assert "STACKPULSE_CONTROL_DIR" not in os.environ
        assert "STACKPULSE_ACTIVITY_FILE" not in os.environ
        reply(request, {"userAgent": "stackpulse-offline-fixture"})
    elif method == "thread/start":
        assert params["approvalPolicy"] == "never"
        reply(request, {"thread": {"id": ROOT, "parentThreadId": None}, "model": "gpt-6-astra", "reasoningEffort": "medium"})
        if mode == "blocked_stdin":
            time.sleep(10)
            break
    elif method == "turn/start":
        reply(request, {"turn": {"id": TURN, "status": "inProgress"}})
        event("turn/started", {"threadId": ROOT, "turn": {"id": TURN, "status": "inProgress"}})
        event("thread/started", {"thread": {"id": CHILD, "parentThreadId": ROOT, "preview": "Verificar validações e testes", "name": "Revisar validações", "model": "gpt-5.6-luna", "reasoningEffort": "max"}})
        event("item/completed", {"threadId": ROOT, "item": {"type": "subAgentActivity", "id": "spawn", "kind": "started", "agentThreadId": CHILD, "agentPath": "/root/worker"}})
        event("turn/started", {"threadId": CHILD, "turn": {"id": "child-turn", "status": "inProgress"}})
        event("item/started", {"threadId": CHILD, "item": {"type": "commandExecution", "id": "command-1", "command": "cargo test --lib", "status": "inProgress"}})
        for total in (100, 120):
            event("thread/tokenUsage/updated", {"threadId": ROOT, "turnId": TURN, "tokenUsage": {"total": {"inputTokens": total, "cachedInputTokens": 10, "outputTokens": 20, "reasoningOutputTokens": 5}}})
        # Must never be surfaced by StackPulse observers or included in final output.
        event("rawResponseItem/completed", {"threadId": ROOT, "item": {"type": "reasoning", "content": ["PRIVATE_FIXTURE_REASONING"]}})
        threading.Thread(target=delayed_complete, daemon=True).start()
    elif method == "turn/steer":
        assert params["threadId"] == ROOT
        assert params["expectedTurnId"] == TURN
        if mode == "reject":
            emit({"id": request["id"], "error": {"code": -32600, "message": "no active turn"}})
        else:
            reply(request, {"turnId": "unexpected-turn" if mode == "malformed_ack" else TURN})
            if mode not in ("unconfirmed", "malformed_ack"):
                text = params["input"][0]["text"]
                target = re.search(r"Target existing subagent ID: ([^.\s]+)\.", text).group(1)
                guidance = text.split("Guidance to forward:\n", 1)[1]
                if "First interrupt only" in text:
                    native_call(request["id"], "interrupt_agent", {"target": target}, "interrupted")
                native_call(request["id"], "followup_task" if "First interrupt only" in text else "send_message", {"target": target, "message": guidance}, "interacted")
                event("item/started", {"threadId": CHILD, "item": {"type": "commandExecution", "id": "command-2", "command": "cargo test validation --lib", "status": "inProgress"}})
        if config.get("finish_after_control"):
            complete()
    elif method == "initialized":
        pass
