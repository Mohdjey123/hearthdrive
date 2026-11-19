// Passkeys through the browser's WebAuthn API. The service checks every
// answer; this only converts between base64url and the browser's buffers.

import type { PasskeyAnswer, PasskeyOptions } from '../services/api'

function b64(buf: ArrayBuffer): string {
  let bin = ''
  for (const b of new Uint8Array(buf)) bin += String.fromCharCode(b)
  return btoa(bin).replace(/\+/g, '-').replace(/\//g, '_').replace(/=+$/, '')
}

function unb64(text: string): Uint8Array<ArrayBuffer> {
  const bin = atob(text.replace(/-/g, '+').replace(/_/g, '/') + '='.repeat((4 - (text.length % 4)) % 4))
  const out = new Uint8Array(bin.length)
  for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i)
  return out
}

/** Whether this browser can use passkeys at all. */
export const passkeysSupported = () => typeof window !== 'undefined' && typeof window.PublicKeyCredential !== 'undefined'

/** Passkeys are made for "localhost": a page at 127.0.0.1 cannot use them. */
export const onLoopbackAddress = () => window.location.hostname === '127.0.0.1' || window.location.hostname === '[::1]'

/** This page at http://localhost, where passkeys work. */
export const localhostUrl = () => `${window.location.protocol}//localhost${window.location.port ? `:${window.location.port}` : ''}${window.location.pathname}`

/** Makes a new passkey (Windows Hello, a phone or a security key). */
export async function createPasskey(o: PasskeyOptions): Promise<{ client_data: string; attestation_object: string }> {
  const cred = (await navigator.credentials.create({
    publicKey: {
      challenge: unb64(o.challenge),
      rp: { id: o.rp.id, name: o.rp.name },
      user: { id: unb64(o.user.id), name: o.user.name, displayName: o.user.display_name },
      pubKeyCredParams: o.algorithms.map((alg) => ({ type: 'public-key' as const, alg })),
      excludeCredentials: o.exclude.map((id) => ({ type: 'public-key' as const, id: unb64(id) })),
      authenticatorSelection: { residentKey: 'required', requireResidentKey: true, userVerification: 'required' },
      attestation: 'none',
      timeout: 120_000,
    },
  })) as PublicKeyCredential | null
  if (!cred) throw new Error('No passkey was made')
  const r = cred.response as AuthenticatorAttestationResponse
  return { client_data: b64(r.clientDataJSON), attestation_object: b64(r.attestationObject) }
}

/** Signs a sign-in challenge with one of this person's passkeys (the browser asks which). */
export async function signWithPasskey(challenge: string, rpId: string): Promise<PasskeyAnswer> {
  const cred = (await navigator.credentials.get({
    publicKey: { challenge: unb64(challenge), rpId, userVerification: 'required', timeout: 120_000 },
  })) as PublicKeyCredential | null
  if (!cred) throw new Error('No passkey was used')
  const r = cred.response as AuthenticatorAssertionResponse
  return {
    credential_id: b64(cred.rawId),
    client_data: b64(r.clientDataJSON),
    authenticator_data: b64(r.authenticatorData),
    signature: b64(r.signature),
    user_handle: r.userHandle ? b64(r.userHandle) : null,
  }
}

/** A sentence for a passkey the browser refused or the person cancelled. */
export function passkeyError(e: unknown): string {
  if (e instanceof DOMException) {
    if (e.name === 'NotAllowedError') return 'Cancelled, or the passkey did not answer in time.'
    if (e.name === 'InvalidStateError') return 'This passkey is already added.'
    if (e.name === 'SecurityError') return 'Passkeys do not work on this page. Open HearthDrive at http://localhost instead.'
    if (e.name === 'NotSupportedError') return 'This browser or device cannot make a passkey HearthDrive can use.'
  }
  return e instanceof Error ? e.message : String(e)
}
