import os from "os";
import path from "path";
import { spawn, spawnSync } from "child_process";
import { fileURLToPath } from "url";

const __dirname = fileURLToPath(new URL(".", import.meta.url));

// Name of the built binary (matches `name` in src-tauri/Cargo.toml).
const APP_BINARY = "iroh-http-tauri-tests";

// keep track of the `tauri-driver` child process
let tauriDriver;
let exiting = false;

export const config = {
  host: "127.0.0.1",
  port: 4444,

  specs: ["./e2e/specs/**/*.js"],
  maxInstances: 1,

  capabilities: [
    {
      maxInstances: 1,
      "tauri:options": {
        application: path.resolve(
          __dirname,
          "src-tauri",
          "target",
          "debug",
          APP_BINARY,
        ),
      },
    },
  ],

  reporters: ["spec"],
  framework: "mocha",
  // The compliance suites (stress, sessions, discovery) create real QUIC nodes
  // and exercise networking, so allow a generous ceiling.
  mochaOpts: {
    ui: "bdd",
    timeout: 180000,
  },

  // Build the Tauri app (debug, no installer bundle) before the session so the
  // binary referenced above exists.
  onPrepare: () => {
    const result = spawnSync(
      "npm",
      ["run", "tauri", "build", "--", "--debug", "--no-bundle"],
      {
        cwd: __dirname,
        stdio: "inherit",
        shell: true,
      },
    );
    if (result.status !== 0) {
      throw new Error(`tauri build failed with exit code ${result.status}`);
    }
  },

  // Start `tauri-driver` so it can proxy WebDriver requests to the native
  // WebKitWebDriver server on Linux.
  beforeSession: () => {
    tauriDriver = spawn(
      path.resolve(os.homedir(), ".cargo", "bin", "tauri-driver"),
      [],
      { stdio: [null, process.stdout, process.stderr] },
    );

    tauriDriver.on("error", (error) => {
      console.error("tauri-driver error:", error);
      process.exit(1);
    });
    tauriDriver.on("exit", (code) => {
      if (!exiting) {
        console.error("tauri-driver exited with code:", code);
        process.exit(1);
      }
    });
  },

  afterSession: () => {
    closeTauriDriver();
  },
};

function closeTauriDriver() {
  exiting = true;
  tauriDriver?.kill();
}

function onShutdown(fn) {
  const cleanup = () => {
    try {
      fn();
    } finally {
      process.exit();
    }
  };
  process.on("exit", cleanup);
  process.on("SIGINT", cleanup);
  process.on("SIGTERM", cleanup);
  process.on("SIGHUP", cleanup);
  process.on("SIGBREAK", cleanup);
}

onShutdown(() => closeTauriDriver());
