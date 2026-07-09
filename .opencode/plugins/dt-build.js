/**
 * dt-build Plugin: 文件修改后自动触发 dt build 更新代码索引
 *
 * 使用 tool.execute.after 钩子拦截 edit/write 工具，
 * 3 秒防抖后批量执行 dt build。
 *
 * 日志输出到: /tmp/dt-build-plugin.log
 */

import { appendFileSync } from "node:fs";

const LOG = "/tmp/dt-build-plugin.log";
const SOURCE_EXT = /\.(java|py|ts|js|tsx|jsx|go|rs|cpp|c|h|vue|svelte)$/;
const DEBOUNCE_MS = 3000;

function log(msg) {
  const ts = new Date().toISOString();
  appendFileSync(LOG, `[${ts}] ${msg}\n`);
}

const DtBuildPlugin = async ({ $ }) => {
  const changedFiles = new Set();
  let timer = null;
  log("插件已加载");

  function scheduleBuild() {
    if (timer) clearTimeout(timer);
    timer = setTimeout(async () => {
      const files = [...changedFiles];
      changedFiles.clear();
      timer = null;
      if (files.length === 0) return;

      log(`触发 dt build，共 ${files.length} 个文件`);
      for (const file of files) {
        try {
          await $`dt build --file ${file}`;
          log(`  ✓ ${file}`);
        } catch (err) {
          log(`  ✗ ${file}: ${err}`);
        }
      }
      log("完成");
    }, DEBOUNCE_MS);
  }

  return {
    // 核心钩子：拦截 edit / write 工具
    "tool.execute.after": async (input) => {
      if (input.tool !== "edit" && input.tool !== "write") return;

      const filePath = input.args?.filePath;
      if (!filePath) return;
      if (!SOURCE_EXT.test(filePath)) return;

      log(`tool.after: ${input.tool} → ${filePath}`);
      changedFiles.add(filePath);
      scheduleBuild();
    },

    // 兜底：会话空闲时也触发一次
    event: async ({ event }) => {
      if (event.type === "session.idle" && changedFiles.size > 0) {
        log(`session.idle 兜底: 已收集 ${changedFiles.size} 个文件`);
        scheduleBuild();
      }
    },

    dispose: async () => {
      if (timer) clearTimeout(timer);
      log("插件已卸载");
      changedFiles.clear();
    },
  };
};

export { DtBuildPlugin as server };
