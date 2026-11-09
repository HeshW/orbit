// Orbit / mock/users: Windows sessions and session actions for the browser preview
// (users feature, docs/V04-CONTRACT.md). Session 1 is the owner; session 2 is a second,
// disconnected account, so the "other user needs admin" path can be previewed.
// URL flags: ?usersfail (list_sessions throws), ?nousers (nobody signed in),
// ?rdpuser (a third, remote session with a client name).

const nowS = () => Math.floor(Date.now() / 1000);
const params = typeof location !== 'undefined' ? new URLSearchParams(location.search) : null;
const flag = (name) => !!params && params.has(name);

let sessions = null;

function list() {
  if (!sessions) {
    sessions = flag('nousers') ? [] : [
      { sessionId: 1, userName: 'Hesh', domain: 'LEGACY-PC', state: 'active', clientName: null, logonTime: nowS() - (2 * 86400 + 5 * 3600 + 15 * 60), isCurrent: true },
      { sessionId: 2, userName: 'Sam', domain: 'LEGACY-PC', state: 'disconnected', clientName: null, logonTime: nowS() - 26 * 3600, isCurrent: false },
    ];
    if (flag('rdpuser')) {
      sessions.push({ sessionId: 3, userName: 'Studio', domain: 'LEGACY-PC', state: 'active', clientName: 'STUDIO-LAPTOP', logonTime: nowS() - 40 * 60, isCurrent: false });
    }
  }
  return sessions;
}

export default {
  async list_sessions(_args, ctx) {
    await ctx.delay(120);
    if (flag('usersfail')) throw 'Could not list the sessions (error 1722).';
    return list().map((s) => ({ ...s }));
  },

  async session_action({ sessionId, action, message }, ctx) {
    if (!['disconnect', 'signout', 'message'].includes(action)) throw `Unknown session action: ${action}.`;
    if (Number(sessionId) === 0) throw "Session 0 runs Windows services. Orbit won't change it.";
    if (action === 'message') {
      const text = String(message || '').trim();
      if (!text) throw 'Type a message first.';
      if (text.length > 1000) throw 'Keep the message under 1000 characters.';
    }
    const s = list().find((x) => x.sessionId === Number(sessionId));
    if (!s) throw 'That session has ended.';
    if (!s.isCurrent && !ctx.isAdmin()) throw 'Access denied. Try Restart as administrator.';
    await ctx.delay(300);
    if (action === 'message') return null;
    // The preview never signs out or disconnects the session it runs in: there is nothing
    // to come back to. The real app does what was asked after the confirm dialog.
    if (s.isCurrent) return null;
    if (action === 'disconnect') s.state = 'disconnected';
    else sessions = sessions.filter((x) => x !== s);
    return null;
  },
};
