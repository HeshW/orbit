// Orbit / mock registry: merges every feature's mock command handlers for the browser
// preview (docs/V04-CONTRACT.md "Mock registry"). api.js imports this file only.
//
// Each module in this folder default-exports { command_name(args, ctx) { ... } }.
// ctx = { snapshot(): the latest mock Snapshot, isAdmin(): boolean, delay(ms): Promise,
// emit(event, payload): send to api.js onEvent subscribers, call(cmd, args) }.
// A handler returns the value (or a Promise of it) or throws a short human string, like
// the Rust command. Registry handlers win over api.js's built-in ones with the same name.
// Mock modules must not import api.js.

import data, { decorateRow, decorateSnapshot } from './data.js';
import security from './security.js';
import autoruns from './autoruns.js';
import network from './network.js';
import users from './users.js';
import history from './history.js';
import tools from './tools.js';
import shell from './shell.js';
import health from './health.js';
import cleanup from './cleanup.js';
import boost from './boost.js';

const modules = { data, security, autoruns, network, users, history, tools, shell, health, cleanup, boost };

export const handlers = {};
for (const [name, mod] of Object.entries(modules)) {
  for (const [cmd, fn] of Object.entries(mod || {})) {
    if (typeof fn !== 'function') continue;
    if (cmd in handlers) console.warn(`[orbit mock] ${name} redefines ${cmd}`);
    handlers[cmd] = fn;
  }
}

export { decorateRow, decorateSnapshot };
