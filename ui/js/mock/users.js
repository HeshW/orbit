// Orbit / mock/users: Windows sessions and session actions for the browser preview
// (users feature, docs/V04-CONTRACT.md). Session 1 is the owner; session 2 is a second,
// disconnected account, so the "other user needs admin" path can be previewed.

const nowS = () => Math.floor(Date.now() / 1000);

let sessions = null;

function list() {
  if (!sessions) {
    sessions = [
      { sessionId: 1, userName: 'Hesh', domain: 'LEGACY-PC', state: 'active', clientName: null, logonTime: nowS() - (2 * 86400 + 5 * 3600 + 15 * 60), isCurrent: true },
      { sessionId: 2, userName: 'Sam', domain: 'LEGACY-PC', state: 'disconnected', clientName: null, logonTime: nowS() - 26 * 3600, isCurrent: false },
    ];
  }
  return sessions;
}

export default {
  list_sessions() {
    return list();
  },

  async session_action({ sessionId, action, message }, ctx) {
    const s = list().find((x) => x.sessionId === Number(sessionId));
    if (!s) throw 'That session has ended.';
    if (!['disconnect', 'signout', 'message'].includes(action)) throw `Unknown action: ${action}`;
    if (!s.isCurrent && !ctx.isAdmin()) throw 'Access denied. Try Restart as administrator.';
    await ctx.delay(300);
    if (action === 'message') {
      if (!message || !String(message).trim()) throw 'Type a message first.';
      return null;
    }
    // The preview never signs out or disconnects the session it runs in: there is nothing
    // to come back to. The real app does what was asked after the confirm dialog.
    if (s.isCurrent) return null;
    if (action === 'disconnect') s.state = 'disconnected';
    else sessions = sessions.filter((x) => x !== s);
    return null;
  },
};
