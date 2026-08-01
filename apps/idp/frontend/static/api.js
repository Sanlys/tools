// Shared helpers: a small fetch wrapper and the base64url <-> ArrayBuffer
// conversions WebAuthn's browser API needs. webauthn-rs's own JSON wire
// format already encodes every binary field (challenge, credential ids,
// user.id, ...) as a base64url *string* -- the browser's
// `navigator.credentials.create/get` API wants those same fields as real
// `ArrayBuffer`s, and the reverse on the way back. This file is the one
// place that translation happens; every page below just calls
// `startRegistration`/`finishRegistration`/`startAuthentication`.

async function api(path, options = {}) {
  const resp = await fetch(path, {
    method: options.method || 'GET',
    headers: { 'Content-Type': 'application/json', Accept: 'application/json', ...(options.headers || {}) },
    body: options.body ? JSON.stringify(options.body) : undefined,
    credentials: 'same-origin',
  });
  const text = await resp.text();
  const data = text ? JSON.parse(text) : null;
  if (!resp.ok) {
    const message = (data && (data.error_description || data.error)) || `request failed (${resp.status})`;
    throw new Error(message);
  }
  return data;
}

function base64urlToBuffer(b64url) {
  const padded = b64url.replace(/-/g, '+').replace(/_/g, '/').padEnd(b64url.length + ((4 - (b64url.length % 4)) % 4), '=');
  const binary = atob(padded);
  const bytes = new Uint8Array(binary.length);
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i);
  return bytes.buffer;
}

function bufferToBase64url(buffer) {
  const bytes = new Uint8Array(buffer);
  let binary = '';
  for (let i = 0; i < bytes.length; i++) binary += String.fromCharCode(bytes[i]);
  return btoa(binary).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '');
}

function decodeCreationOptions(publicKey) {
  return {
    ...publicKey,
    challenge: base64urlToBuffer(publicKey.challenge),
    user: { ...publicKey.user, id: base64urlToBuffer(publicKey.user.id) },
    excludeCredentials: (publicKey.excludeCredentials || []).map((c) => ({ ...c, id: base64urlToBuffer(c.id) })),
  };
}

function decodeRequestOptions(publicKey) {
  return {
    ...publicKey,
    challenge: base64urlToBuffer(publicKey.challenge),
    allowCredentials: (publicKey.allowCredentials || []).map((c) => ({ ...c, id: base64urlToBuffer(c.id) })),
  };
}

function encodeAttestationCredential(cred) {
  return {
    id: cred.id,
    rawId: bufferToBase64url(cred.rawId),
    type: cred.type,
    response: {
      attestationObject: bufferToBase64url(cred.response.attestationObject),
      clientDataJSON: bufferToBase64url(cred.response.clientDataJSON),
    },
    extensions: cred.getClientExtensionResults ? cred.getClientExtensionResults() : {},
  };
}

function encodeAssertionCredential(cred) {
  return {
    id: cred.id,
    rawId: bufferToBase64url(cred.rawId),
    type: cred.type,
    response: {
      authenticatorData: bufferToBase64url(cred.response.authenticatorData),
      clientDataJSON: bufferToBase64url(cred.response.clientDataJSON),
      signature: bufferToBase64url(cred.response.signature),
      userHandle: cred.response.userHandle ? bufferToBase64url(cred.response.userHandle) : null,
    },
    extensions: cred.getClientExtensionResults ? cred.getClientExtensionResults() : {},
  };
}

async function startRegistration(username, displayName, inviteToken, label) {
  const { challenge_id, options } = await api('/api/register/start', {
    method: 'POST',
    body: { username, display_name: displayName || undefined, invite_token: inviteToken || undefined },
  });
  const publicKey = decodeCreationOptions(options.publicKey);
  const credential = await navigator.credentials.create({ publicKey });
  return api('/api/register/finish', {
    method: 'POST',
    body: { challenge_id, credential: encodeAttestationCredential(credential), label: label || undefined },
  });
}

async function startAuthentication(username) {
  const { challenge_id, options } = await api('/api/auth/start', { method: 'POST', body: { username: username || undefined } });
  const publicKey = decodeRequestOptions(options.publicKey);
  const credential = await navigator.credentials.get({ publicKey });
  return api('/api/auth/finish', {
    method: 'POST',
    body: { challenge_id, credential: encodeAssertionCredential(credential) },
  });
}

async function addPasskey(label) {
  const { challenge_id, options } = await api('/api/passkeys/start', { method: 'POST' });
  const publicKey = decodeCreationOptions(options.publicKey);
  const credential = await navigator.credentials.create({ publicKey });
  return api('/api/passkeys/finish', {
    method: 'POST',
    body: { challenge_id, credential: encodeAttestationCredential(credential), label: label || undefined },
  });
}

function qs(name) {
  return new URLSearchParams(window.location.search).get(name);
}
