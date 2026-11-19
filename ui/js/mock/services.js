// Mock for process_services: groups the mock service list by host PID, like the Rust side.
export default {
  async process_services(_args, ctx) {
    const list = (await ctx.call('list_services')) || [];
    const out = {};
    for (const s of list) {
      if (!s.pid) continue;
      (out[s.pid] ||= []).push({ name: s.name, displayName: s.displayName });
    }
    for (const arr of Object.values(out)) {
      arr.sort((a, b) => a.displayName.localeCompare(b.displayName));
    }
    return out;
  },
};
