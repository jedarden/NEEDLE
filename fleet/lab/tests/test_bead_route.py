#!/usr/bin/env python3
"""Tests for bead-route: the router (fleet/lab/bin/bead), its installer, and
the checkpoint publisher guards.

Every subprocess runs with an isolated HOME and explicit BEAD_ROUTE_* overrides,
a fake real bead that records argv/cwd/stdin/BEAD_ACTOR/path-argument contents
to a log, and a BEAD_ROUTE_SSH stub that runs the router's serve mode locally
under a separate "authority" environment. Nothing touches the live host.

Run: python3 -m unittest discover -s fleet/lab/tests -p 'test_bead_route.py'
"""
import base64
import importlib.machinery
import importlib.util
import json
import os
import pty
import shutil
import subprocess
import sys
import tempfile
import unittest

# Loading the extension-less router as a module must not drop a __pycache__
# beside it in the shared checkout.
sys.dont_write_bytecode = True

HERE = os.path.dirname(os.path.abspath(__file__))
LAB = os.path.dirname(HERE)
ROUTER = os.path.join(LAB, "bin", "bead")
INSTALLER = os.path.join(LAB, "bin", "install-bead-router.sh")
UNWEDGE = os.path.join(LAB, "bin", "needle-checkpoint-unwedge")
SWEEP = os.path.join(LAB, "bin", "needle-checkpoint-sweep")
ROUTES_SRC = os.path.join(LAB, "bead-routes.json")
PYTHON = sys.executable
BASE_PATH = os.pathsep.join([os.path.dirname(PYTHON), os.environ.get("PATH", "/usr/bin:/bin")])
ORIGIN = "jedarden/declarative-config"


def load_router():
    loader = importlib.machinery.SourceFileLoader("bead_route_router", ROUTER)
    spec = importlib.util.spec_from_loader("bead_route_router", loader)
    module = importlib.util.module_from_spec(spec)
    loader.exec_module(module)
    return module


R = load_router()

FAKE_BEAD = r'''#!{python} -I
import json, os, sys
LOG = {log!r}
FILE_FLAGS = {{"--description-file", "--notes-file", "--reason-file",
              "--body-file", "--value-file", "--file", "--checkpoint", "--input"}}
args = sys.argv[1:]
record = {{"argv": args, "cwd": os.getcwd(), "actor": os.environ.get("BEAD_ACTOR"),
          "leak": os.environ.get("BEAD_ROUTE_TEST_LEAK"),
          "scanner": os.environ.get("BEAD_ORG_SECRET_SCANNER"), "files": {{}}, "stdin": None}}
needs = "--fencing-token-stdin" in args
for i, token in enumerate(args):
    name, eq, value = token.partition("=")
    if token in FILE_FLAGS and i + 1 < len(args):
        value = args[i + 1]
    elif not (eq and name in FILE_FLAGS):
        continue
    if value == "-":
        needs = True
        continue
    try:
        with open(value) as handle:
            record["files"][value] = handle.read()
    except OSError as error:
        record["files"][value] = "ERR:" + error.strerror
if needs:
    record["stdin"] = sys.stdin.read()
with open(LOG, "a") as handle:
    handle.write(json.dumps(record) + "\n")
if args[:1] in (["--version"], ["-V"]):
    print({version!r})
elif args[:1] == ["capabilities"]:
    print(json.dumps({{"implementation": "bead-rs", "atomic_claim": True}}))
elif args[:3] == ["sync", "status", "--format"]:
    print(json.dumps({{"relationship": "aligned", "checkpoint_consistent": True}}))
else:
    print("fake-ok")
sys.exit(int(os.environ.get("FAKE_BEAD_EXIT", "0")))
'''

SSH_STUB = r'''#!{python} -I
import json, os, shlex, sys
args = sys.argv[1:]
with open({log!r}, "a") as handle:
    handle.write(json.dumps(args) + "\n")
if os.environ.get("STUB_SSH_MODE") == "unreachable":
    sys.stderr.write("ssh: connect to host codinghome port 22: Connection refused\n")
    sys.exit(255)
sep = args.index("--")
if args[sep - 1] != "codinghome":
    sys.exit(255)
command = shlex.split(" ".join(args[sep + 1:]))
with open({auth_env!r}) as handle:
    env = json.load(handle)
os.execve(command[0], command, env)
'''


def write_exec(path, text):
    with open(path, "w") as handle:
        handle.write(text)
    os.chmod(path, 0o755)


def git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True,
                   env=dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
                            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com"))


def make_repo(path, origin, config_json=True):
    os.makedirs(os.path.join(path, ".beads"))
    if config_json:
        with open(os.path.join(path, ".beads", "config.json"), "w") as handle:
            handle.write("{}\n")
    git(path, "init", "--quiet", "--initial-branch=main")
    git(path, "remote", "add", "origin", origin)
    return path


def read_log(path):
    try:
        with open(path) as handle:
            return [json.loads(line) for line in handle if line.strip()]
    except FileNotFoundError:
        return []


class Fixture(unittest.TestCase):
    """Temp hosts: an authority (codinghome) and a client (lab) side."""

    def setUp(self):
        self.base = os.path.realpath(tempfile.mkdtemp(prefix="bead-route-test-"))
        self.addCleanup(shutil.rmtree, self.base, True)
        b = self.base
        for d in ("auth-home", "auth-run", "client-home", "client-run", "plain", "fakes"):
            os.makedirs(os.path.join(b, d))
        self.auth_runtime = os.path.join(b, "auth-run")
        self.client_runtime = os.path.join(b, "client-run")
        self.auth_clone = make_repo(os.path.join(b, "auth", "declarative-config"),
                                    "https://git.ardenone.com/jedarden/declarative-config.git")
        self.other_clone = make_repo(os.path.join(b, "auth", "dc-second-clone"),
                                     "https://git.ardenone.com/jedarden/declarative-config")
        self.lab_clone = make_repo(os.path.join(b, "lab", "declarative-config"),
                                   "git@git.ardenone.com:JedArden/declarative-config.git")
        self.unrouted = make_repo(os.path.join(b, "lab", "other"),
                                  "https://git.ardenone.com/jedarden/other.git")
        self.plain = os.path.join(b, "plain")
        self.routes = os.path.join(b, "routes.json")
        self.write_routes({
            "version": 1,
            "remote_router": ROUTER,
            "routes": {ORIGIN: {
                "authority_host": "codinghome",
                "authority_path": self.auth_clone,
                "clients": ["lab"],
            }},
        })
        fakes = os.path.join(b, "fakes")
        self.client_log = os.path.join(b, "client-bead.log")
        self.auth_log = os.path.join(b, "auth-bead.log")
        self.ssh_log = os.path.join(b, "ssh.log")
        self.client_fake = os.path.join(fakes, "client-bead")
        self.auth_fake = os.path.join(fakes, "auth-bead")
        write_exec(self.client_fake, FAKE_BEAD.format(
            python=PYTHON, log=self.client_log, version="bead 0.3.0-client-fake"))
        write_exec(self.auth_fake, FAKE_BEAD.format(
            python=PYTHON, log=self.auth_log, version="bead 0.3.0-fake"))
        self.auth_env_file = os.path.join(b, "auth-env.json")
        self.set_auth_env()
        self.ssh_stub = os.path.join(fakes, "ssh")
        write_exec(self.ssh_stub, SSH_STUB.format(
            python=PYTHON, log=self.ssh_log, auth_env=self.auth_env_file))

    def write_routes(self, doc):
        with open(self.routes, "w") as handle:
            json.dump(doc, handle)

    def set_auth_env(self, **extra):
        env = {
            "HOME": os.path.join(self.base, "auth-home"),
            "PATH": BASE_PATH,
            "BEAD_ROUTE_HOST": "codinghome",
            "BEAD_ROUTE_ROUTES": self.routes,
            "BEAD_ROUTE_REAL": self.auth_fake,
            "BEAD_ROUTE_HOST_CONFIG": os.path.join(self.base, "auth-home", "absent.json"),
            "XDG_RUNTIME_DIR": self.auth_runtime,
            "BEAD_ACTOR": "authority-own-actor",
        }
        env.update(extra)
        with open(self.auth_env_file, "w") as handle:
            json.dump(env, handle)
        return env

    def client_env(self, host="lab", **extra):
        env = {
            "HOME": os.path.join(self.base, "client-home"),
            "PATH": BASE_PATH,
            "BEAD_ROUTE_HOST": host,
            "BEAD_ROUTE_ROUTES": self.routes,
            "BEAD_ROUTE_REAL": self.client_fake,
            "BEAD_ROUTE_SSH": self.ssh_stub,
            "BEAD_ROUTE_HOST_CONFIG": os.path.join(self.base, "client-home", "absent.json"),
            "XDG_RUNTIME_DIR": self.client_runtime,
        }
        env.update(extra)
        return env

    def bead(self, args, cwd, host="lab", input=None, stdin=None, **extra):
        kwargs = {"input": input} if input is not None else {"stdin": stdin or subprocess.DEVNULL}
        return subprocess.run([ROUTER] + args, cwd=cwd, env=self.client_env(host, **extra),
                              capture_output=True, timeout=60, **kwargs)

    def serve(self, envelope, **auth_extra):
        env = self.set_auth_env(**auth_extra)
        payload = json.dumps(envelope).encode()
        return subprocess.run([ROUTER, "__bead-route-serve__"], cwd=self.plain, env=env,
                              input=payload, capture_output=True, timeout=60)

    def role(self, directory, host="lab", **extra):
        result = subprocess.run([ROUTER, "__bead-route-role__", directory], cwd=self.plain,
                                env=self.client_env(host, **extra), capture_output=True, timeout=60)
        self.assertEqual(result.returncode, 0, result.stderr)
        return json.loads(result.stdout)

    def assert_nothing_ran(self):
        self.assertEqual(read_log(self.client_log), [])
        self.assertEqual(read_log(self.auth_log), [])

    def leftover_scratch(self):
        return [n for n in os.listdir(self.auth_runtime) if n.startswith("bead-route-")]


class PureLogicTests(unittest.TestCase):
    def test_normalize_origin(self):
        for url in ("https://git.ardenone.com/jedarden/declarative-config.git",
                    "https://git.ardenone.com/jedarden/declarative-config/",
                    "git@git.ardenone.com:jedarden/declarative-config.git",
                    "ssh://git@git.ardenone.com:2222/JedArden/Declarative-Config.git",
                    "/srv/git/jedarden/declarative-config.git"):
            self.assertEqual(R.normalize_origin(url), ORIGIN, url)
        self.assertIsNone(R.normalize_origin("repo"))

    def test_decide_every_role(self):
        route = {"authority_host": "codinghome", "clients": ["lab"]}
        self.assertEqual(R.decide("lab", None, "/a", "/a"), "unrouted")
        self.assertEqual(R.decide("codinghome", route, "/a", "/a"), "authority")
        self.assertEqual(R.decide("codinghome", route, "/b", "/a"), "redirect")
        self.assertEqual(R.decide("lab", route, "/b", "/a"), "client")
        self.assertEqual(R.decide("gmktec", route, "/b", "/a"), "refused")

    def test_validate_routes_rejects_bad_tables(self):
        good = {"version": 1, "routes": {"Owner/Repo": {
            "authority_host": "codinghome", "authority_path": "/x", "clients": ["lab"]}}}
        table = R.validate_routes(good, "t")
        self.assertEqual(table["owner/repo"]["clients"], ["lab"])
        bads = [
            [],
            {"version": 2, "routes": {}},
            {"version": 1},
            {"version": 1, "routes": {"norepo": {"authority_host": "h", "authority_path": "/x"}}},
            {"version": 1, "routes": {"o/r": {"authority_host": "h", "authority_path": "rel"}}},
            {"version": 1, "routes": {"o/r": {"authority_host": "-oProxyCommand=x",
                                               "authority_path": "/x"}}},
            {"version": 1, "routes": {"o/r": {"authority_host": "h", "authority_path": "/x",
                                               "clients": ["h"]}}},
            {"version": 1, "remote_router": "bead", "routes": {}},
        ]
        for doc in bads:
            with self.assertRaises(R.Refusal, msg=doc) as caught:
                R.validate_routes(doc, "t")
            self.assertEqual(caught.exception.code, 78)

    def test_allowlist(self):
        allowed = [
            [], ["--version"], ["-V"], ["--help"], ["help", "sync"],
            ["capabilities", "--profile", "native-v1"],
            ["list", "--ready", "--json", "--limit", "5"], ["ready", "--json"],
            ["show", "x", "--json"], ["claim", "--assignee", "w", "--json"],
            ["claim", "x", "--assignee", "w", "--renew-lease", "--lease-ttl", "60",
             "--fencing-token-stdin", "--json"],
            ["update", "x", "--status", "open"], ["release", "x"], ["close", "x", "--reason", "r"],
            ["reopen", "x"], ["resolve", "x", "--attempt-id", "a"],
            ["comment", "create", "x", "--body", "b"], ["comment", "resolve", "x", "c"],
            ["label", "add", "x", "--label", "l"], ["dep", "add", "a", "b"],
            ["ref", "find", "--namespace", "n", "--value", "v"],
            ["data", "set", "--id", "x", "--namespace", "n", "--schema-ref", "s", "--value", "{}"],
            ["query", "--json", "{}"], ["changes", "--since", "0"], ["why", "--id", "x"],
            ["schema", "list"], ["sync", "status", "--format", "json"],
            ["sync", "reconcile", "--actor", "needle-sync"], ["sync", "flush-only"],
            ["doctor"], ["doctor", "--scope", "secrets", "--format", "json"],
            ["--no-auto-flush", "close", "x", "--reason", "r"],
            ["--acknowledge-secret", "fp", "create", "--title", "t"],
            ["--acknowledge-secret=fp", "--skip-foreign-workspace", "list"],
            ["manifest", "commit", "--input", "m.json"], ["manifest", "dry-run", "--input=m.json"],
        ]
        for argv in allowed:
            R.check_allowed(argv, remote=True)
        refused = [
            ["init"], ["restore", "--source", "s", "--generation", "g", "--actor", "a"],
            ["redact", "--all-blocking"], ["manifest", "frobnicate"],
            ["sync", "import-only", "--input", "x"], ["sync", "fork"], ["sync", "commit"],
            ["sync", "configure"], ["sync", "diff", "a", "b"], ["sync", "bisect"],
            ["sync", "flush-only", "--output", "/tmp/x"], ["sync", "flush-only", "--output=/tmp/x"],
            ["doctor", "--repair"], ["doctor", "--rehearse"], ["doctor", "--starvation-recovery"],
            ["watchdog"], ["frobnicate"], ["--unknown-global", "list"], ["--no-auto-flush"],
            ["dep", "--json", "list"],
        ]
        for argv in refused:
            with self.assertRaises(R.CommandRefused, msg=argv):
                R.check_allowed(argv, remote=True)
        # A host-local checkpoint artifact cannot travel; a redirect may use it.
        with self.assertRaises(R.CommandRefused):
            R.check_allowed(["query", "--checkpoint", "c.json", "--json", "{}"], remote=True)
        R.check_allowed(["query", "--checkpoint", "c.json", "--json", "{}"], remote=False)

    def test_path_arguments(self):
        argv = ["create", "--title", "t", "--description-file", "d.md"]
        path, start = R.check_allowed(argv, remote=True)
        self.assertEqual(R.path_args(argv, path, start, R.INPUT_FILE_FLAGS),
                         [(4, "--description-file", "d.md", False)])
        self.assertFalse(R.needs_stdin(argv, path, start))
        self.assertEqual(R.absolutize(argv, path, start, "/w")[4], "/w/d.md")
        argv = ["close", "x", "--reason-file=-"]
        path, start = R.check_allowed(argv, remote=True)
        self.assertTrue(R.needs_stdin(argv, path, start))
        argv = ["update", "x", "--notes-file=n.txt", "--", "--notes-file", "y"]
        path, start = R.check_allowed(argv, remote=True)
        self.assertEqual(R.absolutize(argv, path, start, "/w"),
                         ["update", "x", "--notes-file=/w/n.txt", "--", "--notes-file", "y"])


class RouterTests(Fixture):
    def test_no_routes_file_is_pure_passthrough(self):
        result = self.bead(["list", "--json"], self.lab_clone,
                           BEAD_ROUTE_ROUTES=os.path.join(self.base, "absent.json"))
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = read_log(self.client_log)
        self.assertEqual([c["argv"] for c in calls], [["list", "--json"]])
        self.assertEqual(calls[0]["cwd"], self.lab_clone)
        self.assertEqual(read_log(self.ssh_log), [])

    def test_unrouted_and_workspaceless_passthrough(self):
        for cwd in (self.unrouted, self.plain):
            result = self.bead(["init"], cwd)
            self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual([c["cwd"] for c in read_log(self.client_log)], [self.unrouted, self.plain])
        self.assertEqual(read_log(self.ssh_log), [])
        self.assertEqual(read_log(self.auth_log), [])

    def test_authority_same_path_has_full_access(self):
        sub = os.path.join(self.auth_clone, "k8s")
        os.makedirs(sub)
        for args in (["init"], ["sync", "import-only", "--input", "x"], ["doctor", "--repair"]):
            result = self.bead(args, sub, host="codinghome")
            self.assertEqual(result.returncode, 0, result.stderr)
        calls = read_log(self.client_log)
        self.assertEqual(len(calls), 3)
        self.assertTrue(all(c["cwd"] == sub for c in calls))
        self.assertEqual(read_log(self.ssh_log), [])

    def test_redirect_from_another_clone(self):
        sub = os.path.join(self.other_clone, "notes")
        os.makedirs(sub)
        with open(os.path.join(sub, "n.txt"), "w") as handle:
            handle.write("redirected notes\n")
        result = self.bead(["update", "x", "--notes-file", "n.txt"], sub, host="codinghome")
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.bead(["update", "x", "--notes-file=n.txt"], sub, host="codinghome")
        self.assertEqual(result.returncode, 0, result.stderr)
        calls = read_log(self.client_log)
        absolute = os.path.join(sub, "n.txt")
        self.assertEqual(calls[0]["cwd"], self.auth_clone)
        self.assertEqual(calls[0]["argv"], ["update", "x", "--notes-file", absolute])
        self.assertEqual(calls[0]["files"][absolute], "redirected notes\n")
        self.assertEqual(calls[1]["argv"], ["update", "x", "--notes-file=" + absolute])
        # The client allowlist applies to a second clone on the authority host.
        for args in (["init"], ["sync", "import-only", "--input", "x"], ["doctor", "--repair"]):
            result = self.bead(args, sub, host="codinghome")
            self.assertEqual(result.returncode, 78, args)
            self.assertIn("authority checkout", result.stderr.decode())
        self.assertEqual(len(read_log(self.client_log)), 2)
        self.assertEqual(read_log(self.ssh_log), [])

    def test_client_list_is_remote(self):
        result = self.bead(["list", "--json"], self.lab_clone, BEAD_ACTOR="lab-worker-7",
                           BEAD_ROUTE_TEST_LEAK="must-not-travel")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.decode().strip(), "fake-ok")
        self.assertEqual(read_log(self.client_log), [])
        calls = read_log(self.auth_log)
        self.assertEqual(len(calls), 1)
        self.assertEqual(calls[0]["argv"], ["list", "--json"])
        self.assertEqual(calls[0]["cwd"], self.auth_clone)
        self.assertEqual(calls[0]["actor"], "lab-worker-7")
        self.assertIsNone(calls[0]["leak"])
        self.assertIsNone(calls[0]["stdin"])
        ssh = read_log(self.ssh_log)[0]
        for option in ("BatchMode=yes", "ConnectTimeout=10", "ServerAliveInterval=15",
                       "ServerAliveCountMax=4"):
            self.assertIn(option, ssh)
        self.assertEqual(ssh[0], "-T")
        self.assertEqual(ssh[ssh.index("--") - 1:], ["codinghome", "--", ROUTER, "__bead-route-serve__"])
        # The request never rides in ssh's argv.
        self.assertNotIn("list", ssh)

    def test_authority_scanner_policy_applies_to_routed_calls(self):
        # The client's own scanner setting never travels.
        result = self.bead(["list"], self.lab_clone, BEAD_ORG_SECRET_SCANNER="/client/scanner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIsNone(read_log(self.auth_log)[0]["scanner"])
        # The authority's environment.d policy reaches the real bead.
        envd = os.path.join(self.base, "auth-home", ".config", "environment.d")
        os.makedirs(envd)
        with open(os.path.join(envd, "50-bead-org-scanner.conf"), "w") as handle:
            handle.write("# org scanner\nBEAD_ORG_SECRET_SCANNER=\"/authority/secret-scanner\"\n")
        result = self.bead(["list"], self.lab_clone, BEAD_ORG_SECRET_SCANNER="/client/scanner")
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(read_log(self.auth_log)[1]["scanner"], "/authority/secret-scanner")
        # An explicit setting in the serve environment wins.
        self.set_auth_env(BEAD_ORG_SECRET_SCANNER="off")
        result = self.bead(["list"], self.lab_clone)
        self.assertEqual(read_log(self.auth_log)[2]["scanner"], "off")

    def test_client_without_actor_does_not_inherit_authority_actor(self):
        result = self.bead(["show", "x"], self.lab_clone)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIsNone(read_log(self.auth_log)[0]["actor"])

    def test_client_create_ships_description_file_and_cleans_up(self):
        with open(os.path.join(self.lab_clone, "desc.md"), "w") as handle:
            handle.write("# Local description\nfrom lab\n")
        for spelling in (["--description-file", "desc.md"], ["--description-file=desc.md"]):
            result = self.bead(["create", "--title", "T"] + spelling, self.lab_clone)
            self.assertEqual(result.returncode, 0, result.stderr)
        calls = read_log(self.auth_log)
        self.assertEqual(len(calls), 2)
        for call in calls:
            self.assertEqual(call["argv"][:4], ["create", "--title", "T", "--description-file"])
            materialized = call["argv"][4]
            self.assertTrue(materialized.startswith(self.auth_runtime + "/bead-route-"), materialized)
            self.assertEqual(call["files"][materialized], "# Local description\nfrom lab\n")
            self.assertFalse(os.path.exists(materialized))
        self.assertEqual(self.leftover_scratch(), [])
        # A missing local file fails on the client, before any transport.
        before = len(read_log(self.ssh_log))
        result = self.bead(["create", "--title", "T", "--description-file", "absent.md"], self.lab_clone)
        self.assertEqual(result.returncode, 3)
        self.assertEqual(len(read_log(self.ssh_log)), before)

    def test_client_manifest_ships_input_file(self):
        # NEEDLE's split operation runs `manifest commit --input` (src/cli/proposals.rs);
        # refusing it would fail every split a client worker attempts.
        with open(os.path.join(self.lab_clone, "m.json"), "w") as handle:
            handle.write('{"children": []}\n')
        for sub in ("dry-run", "commit"):
            result = self.bead(["manifest", sub, "--input", "m.json"], self.lab_clone)
            self.assertEqual(result.returncode, 0, result.stderr)
        calls = read_log(self.auth_log)
        self.assertEqual([call["argv"][:3] for call in calls],
                         [["manifest", "dry-run", "--input"], ["manifest", "commit", "--input"]])
        for call in calls:
            materialized = call["argv"][3]
            self.assertTrue(materialized.startswith(self.auth_runtime + "/bead-route-"), materialized)
            self.assertEqual(call["files"][materialized], '{"children": []}\n')
            self.assertFalse(os.path.exists(materialized))
        self.assertEqual(self.leftover_scratch(), [])

    def test_fencing_token_stdin_is_forwarded(self):
        argv = ["release", "x", "--if-revision", "3", "--fencing-token-stdin"]
        result = self.bead(argv, self.lab_clone, input=b"opaque-token-123\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        call = read_log(self.auth_log)[0]
        self.assertEqual(call["argv"], argv)
        self.assertEqual(call["stdin"], "opaque-token-123\n")
        self.assertEqual(self.leftover_scratch(), [])

    def test_dash_file_value_reads_stdin(self):
        result = self.bead(["close", "x", "--reason-file", "-"], self.lab_clone, input=b"done via stdin\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        result = self.bead(["comment", "create", "x", "--body-file=-"], self.lab_clone, input=b"body\n")
        self.assertEqual(result.returncode, 0, result.stderr)
        first, second = read_log(self.auth_log)
        self.assertEqual(first["argv"], ["close", "x", "--reason-file", "-"])
        self.assertEqual(first["stdin"], "done via stdin\n")
        self.assertEqual(second["argv"], ["comment", "create", "x", "--body-file", "-"])
        self.assertEqual(second["stdin"], "body\n")

    def test_idle_stdin_is_never_read(self):
        proc = subprocess.Popen([ROUTER, "list"], cwd=self.lab_clone, env=self.client_env(),
                                stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        try:
            proc.wait(timeout=30)
        finally:
            proc.stdin.close()
            proc.stdout.close()
            proc.stderr.close()
        self.assertEqual(proc.returncode, 0)
        self.assertIsNone(read_log(self.auth_log)[0]["stdin"])

    def test_tty_stdin_is_never_read(self):
        master, slave = pty.openpty()
        try:
            result = subprocess.run(
                [ROUTER, "release", "x", "--fencing-token-stdin"], cwd=self.lab_clone,
                env=self.client_env(), stdin=slave, capture_output=True, timeout=30)
        finally:
            os.close(master)
            os.close(slave)
        self.assertEqual(result.returncode, 2)
        self.assertIn("terminal", result.stderr.decode())
        self.assertEqual(read_log(self.ssh_log), [])
        self.assert_nothing_ran()

    def test_version_and_capabilities_are_proxied(self):
        result = self.bead(["--version"], self.lab_clone)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.decode().strip(), "bead 0.3.0-fake")
        result = self.bead(["capabilities", "--profile", "native-v1"], self.lab_clone)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(json.loads(result.stdout)["implementation"], "bead-rs")
        calls = read_log(self.auth_log)
        self.assertEqual([c["argv"] for c in calls],
                         [["--version"], ["capabilities", "--profile", "native-v1"]])
        self.assertTrue(all(c["cwd"] == self.auth_clone for c in calls))
        self.assertEqual(read_log(self.client_log), [])

    def test_exit_status_passes_through(self):
        self.set_auth_env(FAKE_BEAD_EXIT="4")
        result = self.bead(["claim", "--assignee", "w", "--json"], self.lab_clone)
        self.assertEqual(result.returncode, 4)
        self.assertEqual(len(read_log(self.auth_log)), 1)

    def test_client_refuses_destructive_commands(self):
        refused = [
            ["init"], ["init", "--prefix", "x"],
            ["restore", "--source", ".beads/checkpoint", "--generation", "g", "--actor", "a"],
            ["redact", "--all-blocking", "--actor", "a", "--reason", "r"],
            ["manifest", "bogus"],
            ["sync", "import-only", "--input", "x", "--merge"], ["sync", "fork"],
            ["sync", "commit"], ["sync", "configure"], ["sync", "diff", "a", "b"],
            ["sync", "bisect", "--checkpoint", "a"],
            ["sync", "flush-only", "--output", "/tmp/x"], ["sync", "flush-only", "--output=/tmp/x"],
            ["doctor", "--repair"], ["doctor", "--rehearse"],
            ["watchdog", "--force"], ["frobnicate"], ["--bogus", "list"],
            ["query", "--checkpoint", "c.json", "--json", "{}"],
        ]
        for argv in refused:
            result = self.bead(argv, self.lab_clone)
            self.assertEqual(result.returncode, 78, (argv, result.stderr))
            self.assertIn("codinghome", result.stderr.decode())
        self.assertEqual(read_log(self.ssh_log), [])
        self.assert_nothing_ran()
        allowed = [["sync", "status", "--format", "json"], ["sync", "flush-only"],
                   ["sync", "reconcile", "--actor", "needle-sync"], ["doctor"],
                   ["doctor", "--scope", "secrets", "--format", "json"]]
        for argv in allowed:
            result = self.bead(argv, self.lab_clone)
            self.assertEqual(result.returncode, 0, (argv, result.stderr))
        self.assertEqual([c["argv"] for c in read_log(self.auth_log)], allowed)

    def test_unlisted_host_is_refused(self):
        for argv in (["list"], ["--version"]):
            result = self.bead(argv, self.lab_clone, host="gmktec")
            self.assertEqual(result.returncode, 78)
            message = result.stderr.decode()
            self.assertIn("codinghome", message)
            self.assertIn("cannot reach", message)
            self.assertIn("listed client", message)
        self.assertEqual(read_log(self.ssh_log), [])
        self.assert_nothing_ran()

    def test_unreachable_authority_never_falls_back(self):
        result = self.bead(["claim", "--assignee", "w", "--json"], self.lab_clone,
                           STUB_SSH_MODE="unreachable")
        self.assertEqual(result.returncode, 75)
        self.assertIn("unreachable", result.stderr.decode())
        self.assertIn("local store", result.stderr.decode())
        self.assert_nothing_ran()

    def test_malformed_routes_fail_closed_everywhere(self):
        for content in ("{not json", json.dumps({"version": 2, "routes": {}})):
            with open(self.routes, "w") as handle:
                handle.write(content)
            for cwd in (self.plain, self.unrouted, self.lab_clone):
                result = self.bead(["--version"], cwd)
                self.assertEqual(result.returncode, 78, (content, cwd))
                self.assertIn("routes file", result.stderr.decode())
            result = subprocess.run([ROUTER, "__bead-route-role__", self.plain], env=self.client_env(),
                                    capture_output=True, timeout=30)
            self.assertEqual(result.returncode, 78)
        self.assert_nothing_ran()

    def test_missing_real_bead_is_127(self):
        env = self.client_env(BEAD_ROUTE_REAL="")
        result = subprocess.run([ROUTER, "--version"], cwd=self.plain, env=env,
                                capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 127)
        self.assertIn("bead-route:", result.stderr.decode())
        # host.json naming the router itself is skipped, never exec'd.
        host_json = os.path.join(self.base, "host.json")
        with open(host_json, "w") as handle:
            json.dump({"real_bead": ROUTER}, handle)
        env["BEAD_ROUTE_HOST_CONFIG"] = host_json
        result = subprocess.run([ROUTER, "--version"], cwd=self.plain, env=env,
                                capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 127)
        with open(host_json, "w") as handle:
            json.dump({"real_bead": self.client_fake}, handle)
        result = subprocess.run([ROUTER, "--version"], cwd=self.plain, env=env,
                                capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertEqual(result.stdout.decode().strip(), "bead 0.3.0-client-fake")

    def test_role_query_reports_each_role(self):
        cases = [
            (self.plain, "lab", "unrouted", None),
            (self.unrouted, "lab", "unrouted", "jedarden/other"),
            (self.auth_clone, "codinghome", "authority", ORIGIN),
            (self.other_clone, "codinghome", "redirect", ORIGIN),
            (self.lab_clone, "lab", "client", ORIGIN),
            (self.lab_clone, "gmktec", "refused", ORIGIN),
        ]
        for directory, host, role, origin in cases:
            doc = self.role(directory, host)
            self.assertEqual(doc["role"], role, (directory, host))
            self.assertEqual(doc["origin"], origin)
            if origin == ORIGIN:
                self.assertEqual(doc["authority_host"], "codinghome")
                self.assertEqual(doc["authority_path"], self.auth_clone)
            else:
                self.assertIsNone(doc["authority_host"])
        doc = self.role(self.lab_clone, "lab", BEAD_ROUTE_ROUTES=os.path.join(self.base, "absent"))
        self.assertEqual((doc["role"], doc["origin"]), ("unrouted", ORIGIN))


class ServeTests(Fixture):
    def envelope(self, argv, **overrides):
        envelope = {"v": 1, "client_host": "lab", "workspace_path": self.auth_clone,
                    "argv": argv, "files": {}, "slots": [], "stdin": None, "env": {}}
        envelope.update(overrides)
        return envelope

    def test_serve_runs_an_allowed_request(self):
        payload = base64.b64encode(b"shipped").decode()
        result = self.serve(self.envelope(
            ["create", "--title", "t", "--description-file", "bead-route-slot:f0"],
            files={"f0": payload}, slots=[[4, "f0"]], env={"BEAD_ACTOR": "lab-w"}))
        self.assertEqual(result.returncode, 0, result.stderr)
        call = read_log(self.auth_log)[0]
        self.assertEqual(call["cwd"], self.auth_clone)
        self.assertEqual(call["actor"], "lab-w")
        self.assertEqual(list(call["files"].values()), ["shipped"])
        self.assertEqual(self.leftover_scratch(), [])

    def test_serve_rejects_unlisted_client(self):
        result = self.serve(self.envelope(["list"], client_host="gmktec"))
        self.assertEqual(result.returncode, 78)
        self.assertIn("not a client", result.stderr.decode())
        self.assert_nothing_ran()

    def test_serve_rejects_workspace_mismatch(self):
        for workspace in (self.other_clone, self.unrouted, self.plain, "relative/path"):
            result = self.serve(self.envelope(["list"], workspace_path=workspace))
            self.assertEqual(result.returncode, 78, workspace)
        self.assert_nothing_ran()

    def test_serve_requires_this_host_to_be_the_authority(self):
        result = self.serve(self.envelope(["list"]), BEAD_ROUTE_HOST="lab")
        self.assertEqual(result.returncode, 78)
        self.assert_nothing_ran()

    def test_serve_reapplies_the_allowlist_and_file_slots(self):
        bad = [
            self.envelope(["init"]),
            self.envelope(["doctor", "--repair"]),
            self.envelope(["sync", "flush-only", "--output", "/tmp/x"]),
            # An authority-local path that did not travel with the request.
            self.envelope(["create", "--title", "t", "--description-file", "/etc/hostname"]),
            self.envelope(["create", "--title", "t", "--description-file=/etc/hostname"]),
            # A slot that does not name an input-file argument.
            self.envelope(["list", "bead-route-slot:f0"], files={"f0": "eA=="}, slots=[[1, "f0"]]),
            # A slot naming a file that was not sent.
            self.envelope(["close", "x", "--reason-file", "bead-route-slot:f1"], slots=[[3, "f1"]]),
            {"v": 2, "argv": ["list"]},
        ]
        for envelope in bad:
            result = self.serve(envelope)
            self.assertEqual(result.returncode, 78, envelope)
        payload = b"not json"
        result = subprocess.run([ROUTER, "__bead-route-serve__"], env=self.set_auth_env(),
                                input=payload, capture_output=True, timeout=30)
        self.assertEqual(result.returncode, 78)
        self.assert_nothing_ran()
        self.assertEqual(self.leftover_scratch(), [])


class PublisherGuardTests(Fixture):
    def setUp(self):
        super().setUp()
        self.path_bin = os.path.join(self.base, "path-bin")
        os.makedirs(self.path_bin)
        self.path_log = os.path.join(self.base, "path-bead.log")
        write_exec(os.path.join(self.path_bin, "bead"), FAKE_BEAD.format(
            python=PYTHON, log=self.path_log, version="bead 0.3.0-path-fake"))

    def guard_env(self, host, **extra):
        env = {
            "HOME": os.path.join(self.base, "client-home"),
            "PATH": os.pathsep.join([self.path_bin, BASE_PATH]),
            "BEAD_ROUTE_HOST": host,
            "BEAD_ROUTE_ROUTES": self.routes,
            "BEAD_ROUTE_ROUTER": ROUTER,
            "BEAD_ROUTE_REAL": self.client_fake,
            "BEAD_ROUTE_HOST_CONFIG": os.path.join(self.base, "absent.json"),
        }
        env.update(extra)
        return env

    def run_tool(self, argv, env):
        return subprocess.run(argv, env=env, cwd=self.plain, capture_output=True, text=True, timeout=60)

    def test_unwedge_skips_a_client_workspace(self):
        result = self.run_tool([UNWEDGE, "--workspace", self.lab_clone], self.guard_env("lab"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("routed to codinghome", result.stdout)
        self.assertEqual(read_log(self.path_log), [])

    def test_unwedge_fails_safe_when_the_role_query_fails(self):
        broken = os.path.join(self.base, "broken-router")
        write_exec(broken, "#!/bin/sh\nexit 1\n")
        env = self.guard_env("lab", BEAD_ROUTE_ROUTER=broken)
        result = self.run_tool([UNWEDGE, "--workspace", self.auth_clone], env)
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("route-unknown", result.stdout)
        self.assertEqual(read_log(self.path_log), [])

    def test_unwedge_keeps_existing_behavior_for_authority_and_without_routes(self):
        for env in (self.guard_env("codinghome"),
                    self.guard_env("lab", BEAD_ROUTE_ROUTES=os.path.join(self.base, "absent"))):
            target = self.auth_clone if env["BEAD_ROUTE_HOST"] == "codinghome" else self.lab_clone
            result = self.run_tool([UNWEDGE, "--workspace", target, "--dry-run"], env)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertIn("aligned checkpoint", result.stdout)
        self.assertEqual([c["argv"] for c in read_log(self.path_log)],
                         [["sync", "status", "--format", "json"]] * 2)

    def test_sweep_skips_a_client_workspace(self):
        result = self.run_tool([SWEEP, "--dry-run", "--workspace", self.lab_clone], self.guard_env("lab"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertIn("%s: routed routed to codinghome" % self.lab_clone, result.stdout)
        self.assertEqual(read_log(self.path_log), [])
        # The authority's own checkout keeps the existing classification path.
        result = self.run_tool([SWEEP, "--dry-run", "--workspace", self.auth_clone],
                               self.guard_env("codinghome"))
        self.assertEqual(result.returncode, 0, result.stderr)
        self.assertNotIn("routed", result.stdout)


class InstallerTests(unittest.TestCase):
    FAKE = "#!/bin/sh\nif [ \"$1\" = --version ]; then echo 'bead 9.9.9-fake'; else echo fake \"$@\"; fi\n"

    def setUp(self):
        self.home = os.path.realpath(tempfile.mkdtemp(prefix="bead-route-install-"))
        self.addCleanup(shutil.rmtree, self.home, True)
        os.makedirs(os.path.join(self.home, ".local", "bin"))
        self.target = os.path.join(self.home, ".local", "bin", "bead")
        self.real = os.path.join(self.home, ".local", "lib", "bead-route", "bead-real")
        self.conf = os.path.join(self.home, ".config", "bead-route")

    def env(self):
        return {"HOME": self.home, "PATH": BASE_PATH, "LANG": "C.UTF-8"}

    def install(self, *args):
        return subprocess.run(["bash", INSTALLER, *args], env=self.env(), cwd=self.home,
                              capture_output=True, text=True, timeout=120)

    def read(self, path):
        with open(path, "rb") as handle:
            return handle.read()

    def test_dry_run_changes_nothing(self):
        write_exec(self.target, self.FAKE)
        result = self.install("--dry-run")
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("[dry-run]", result.stdout)
        self.assertEqual(self.read(self.target), self.FAKE.encode())
        self.assertFalse(os.path.exists(self.conf))
        self.assertFalse(os.path.exists(os.path.dirname(self.real)))

    def test_relative_source_paths_are_resolved(self):
        # Deploying from a copied directory (`install-bead-router.sh bin/bead
        # bead-routes.json`) failed: validation runs from `cd /`.
        staged = os.path.join(self.home, "staged", "bin")
        os.makedirs(staged)
        shutil.copy(ROUTER, os.path.join(staged, "bead"))
        shutil.copy(ROUTES_SRC, os.path.join(self.home, "staged", "bead-routes.json"))
        write_exec(self.target, self.FAKE)
        result = subprocess.run(["bash", INSTALLER, "--dry-run", "bin/bead", "bead-routes.json"],
                                env=self.env(), cwd=os.path.join(self.home, "staged"),
                                capture_output=True, text=True, timeout=120)
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertIn("routing table valid", result.stdout + result.stderr)

    def test_install_moves_the_real_binary_aside_and_leaves_a_working_router(self):
        write_exec(self.target, self.FAKE)
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.read(self.target), self.read(ROUTER))
        self.assertTrue(os.access(self.target, os.X_OK))
        self.assertEqual(self.read(self.real), self.FAKE.encode())
        with open(os.path.join(self.conf, "host.json")) as handle:
            self.assertEqual(json.load(handle), {"real_bead": self.real})
        self.assertEqual(self.read(os.path.join(self.conf, "routes.json")), self.read(ROUTES_SRC))
        version = subprocess.run([self.target, "--version"], env=self.env(), cwd="/",
                                 capture_output=True, text=True, timeout=30)
        self.assertEqual((version.returncode, version.stdout.strip()), (0, "bead 9.9.9-fake"))
        self.assertEqual([n for n in os.listdir(os.path.dirname(self.target)) if n != "bead"], [])

        # Idempotent re-run: the router is recognized, bead-real reused.
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.read(self.real), self.FAKE.encode())
        self.assertEqual(os.listdir(os.path.dirname(self.real)), ["bead-real"])

        # A bead upgrade that clobbers the router: the new binary becomes
        # bead-real and the previous one is kept as a timestamped backup.
        upgraded = self.FAKE.replace("9.9.9", "9.9.10")
        write_exec(self.target, upgraded)
        result = self.install()
        self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
        self.assertEqual(self.read(self.target), self.read(ROUTER))
        self.assertEqual(self.read(self.real), upgraded.encode())
        backups = [n for n in os.listdir(os.path.dirname(self.real)) if n.startswith("bead-real.bak-")]
        self.assertEqual(len(backups), 1)
        version = subprocess.run([self.target, "--version"], env=self.env(), cwd="/",
                                 capture_output=True, text=True, timeout=30)
        self.assertEqual(version.stdout.strip(), "bead 9.9.10-fake")

    def test_install_refuses_without_a_real_bead(self):
        result = self.install()
        self.assertNotEqual(result.returncode, 0)
        self.assertIn("no real bead binary", result.stderr)
        self.assertFalse(os.path.exists(self.target))
        self.assertFalse(os.path.exists(self.conf))

    def test_install_rejects_an_invalid_routing_table(self):
        write_exec(self.target, self.FAKE)
        bad = os.path.join(self.home, "bad-routes.json")
        with open(bad, "w") as handle:
            json.dump({"version": 1, "routes": {"o/r": {"authority_host": "h",
                                                         "authority_path": "relative"}}}, handle)
        result = self.install(ROUTER, bad)
        self.assertNotEqual(result.returncode, 0)
        self.assertEqual(self.read(self.target), self.FAKE.encode())
        self.assertFalse(os.path.exists(self.conf))


if __name__ == "__main__":
    unittest.main()
