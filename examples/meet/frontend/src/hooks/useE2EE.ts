import { useRef, useEffect, useCallback, useState } from 'react'
import type { UseSignalingReturn } from '@/hooks/useSignaling'
import type { SignalingMessage } from '@/types'

// ── Types ──────────────────────────────────────────────────────────────

export interface E2EEPeerState {
  ready: boolean
  keyId: number
}

export interface UseE2EEOptions {
  enabled: boolean
  participantId: string
  roomId: string
  signaling: UseSignalingReturn
}

export interface UseE2EEReturn {
  /** The shared Web Worker performing encrypt/decrypt transforms */
  worker: Worker | null
  /** Per-peer E2EE state (ready once sender key exchanged) */
  peerStates: Map<string, E2EEPeerState>
  /** Whether E2EE is enabled for this session */
  enabled: boolean
  /** Handle incoming E2EE signaling messages */
  handleMessage: (msg: SignalingMessage) => void
  /** Notify E2EE of a peer departure (triggers key rotation) */
  onPeerLeft: (peerId: string) => void
  /** Notify E2EE of a new peer arrival */
  onPeerJoined: (peerId: string) => void
  /** Current local key ID */
  localKeyId: number
  /** Encrypt a chat message with our sender key. Returns null if E2EE not ready. */
  encryptChat: (content: string) => Promise<{ ciphertext: string; keyId: number } | null>
  /** Decrypt a chat message from a peer. Returns null if key unavailable. */
  decryptChat: (from: string, ciphertext: string, keyId: number) => Promise<string | null>
  /** Per-peer safety numbers for identity verification (hex fingerprints). */
  safetyNumbers: Map<string, string>
}

// ── Helpers ────────────────────────────────────────────────────────────

async function generateSenderKey(): Promise<ArrayBuffer> {
  const key = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt'])
  return crypto.subtle.exportKey('raw', key)
}

async function generateECDHKeyPair(): Promise<CryptoKeyPair> {
  return crypto.subtle.generateKey({ name: 'ECDH', namedCurve: 'P-256' }, false, ['deriveKey', 'deriveBits'])
}

async function deriveSharedSecret(privateKey: CryptoKey, publicKey: CryptoKey): Promise<CryptoKey> {
  return crypto.subtle.deriveKey(
    { name: 'ECDH', public: publicKey },
    privateKey,
    { name: 'AES-GCM', length: 256 },
    false,
    ['encrypt', 'decrypt'],
  )
}

async function exportPublicKey(key: CryptoKey): Promise<string> {
  const raw = await crypto.subtle.exportKey('raw', key)
  return btoa(String.fromCharCode(...new Uint8Array(raw)))
}

async function importPublicKey(base64: string): Promise<CryptoKey> {
  const binary = atob(base64)
  const bytes = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i++) bytes[i] = binary.charCodeAt(i)
  return crypto.subtle.importKey('raw', bytes, { name: 'ECDH', namedCurve: 'P-256' }, true, [])
}

async function encryptSenderKey(sharedSecret: CryptoKey, senderKeyRaw: ArrayBuffer): Promise<string> {
  const iv = crypto.getRandomValues(new Uint8Array(12))
  const ciphertext = await crypto.subtle.encrypt({ name: 'AES-GCM', iv, tagLength: 128 }, sharedSecret, senderKeyRaw)
  // Pack: [iv 12 bytes] [ciphertext]
  const packed = new Uint8Array(12 + ciphertext.byteLength)
  packed.set(iv, 0)
  packed.set(new Uint8Array(ciphertext), 12)
  return btoa(String.fromCharCode(...packed))
}

async function decryptSenderKey(sharedSecret: CryptoKey, encryptedBase64: string): Promise<ArrayBuffer> {
  const binary = atob(encryptedBase64)
  const packed = new Uint8Array(binary.length)
  for (let i = 0; i < binary.length; i++) packed[i] = binary.charCodeAt(i)
  const iv = packed.slice(0, 12)
  const ciphertext = packed.slice(12)
  return crypto.subtle.decrypt({ name: 'AES-GCM', iv, tagLength: 128 }, sharedSecret, ciphertext)
}

/**
 * Compute a safety number (fingerprint) from two public keys for identity verification.
 * The fingerprint is a SHA-256 hash of the sorted concatenation of both raw public keys,
 * displayed as groups of 5 digits (similar to Signal's Safety Numbers).
 */
async function computeSafetyNumber(localPubKey: CryptoKey, remotePubKey: CryptoKey): Promise<string> {
  const localRaw = new Uint8Array(await crypto.subtle.exportKey('raw', localPubKey))
  const remoteRaw = new Uint8Array(await crypto.subtle.exportKey('raw', remotePubKey))

  // Sort keys lexicographically so both sides compute the same fingerprint
  let first: Uint8Array, second: Uint8Array
  const cmp = compareBytes(localRaw, remoteRaw)
  if (cmp <= 0) {
    first = localRaw; second = remoteRaw
  } else {
    first = remoteRaw; second = localRaw
  }

  const combined = new Uint8Array(first.length + second.length)
  combined.set(first, 0)
  combined.set(second, first.length)

  const hash = new Uint8Array(await crypto.subtle.digest('SHA-256', combined))

  // Format as groups of 5 digits (12 groups = 60 digits, from 32 bytes)
  let digits = ''
  for (let i = 0; i < hash.length; i++) {
    digits += hash[i].toString(10).padStart(3, '0')
  }
  // Take 60 digits, format as 12 groups of 5
  const groups: string[] = []
  for (let i = 0; i < 60; i += 5) {
    groups.push(digits.slice(i, i + 5))
  }
  return groups.join(' ')
}

function compareBytes(a: Uint8Array, b: Uint8Array): number {
  const len = Math.min(a.length, b.length)
  for (let i = 0; i < len; i++) {
    if (a[i] !== b[i]) return a[i] - b[i]
  }
  return a.length - b.length
}

// ── Hook ───────────────────────────────────────────────────────────────

export function useE2EE({ enabled, participantId, roomId, signaling }: UseE2EEOptions): UseE2EEReturn {
  const workerRef = useRef<Worker | null>(null)
  const ecdhKeyPairRef = useRef<CryptoKeyPair | null>(null)
  const senderKeyRawRef = useRef<ArrayBuffer | null>(null)
  const localKeyIdRef = useRef(0)
  const [localKeyId, setLocalKeyId] = useState(0)

  // Per-peer ECDH public keys (received, not yet used for sender key exchange)
  const peerPublicKeysRef = useRef<Map<string, CryptoKey>>(new Map())
  // Per-peer shared secrets
  const sharedSecretsRef = useRef<Map<string, CryptoKey>>(new Map())
  // Peer E2EE states
  const peerStatesRef = useRef<Map<string, E2EEPeerState>>(new Map())
  const [peerStates, setPeerStates] = useState<Map<string, E2EEPeerState>>(new Map())
  // Per-peer decrypted sender keys for chat encryption
  const peerSenderKeysRef = useRef<Map<string, { raw: ArrayBuffer; keyId: number }>>(new Map())
  // Safety numbers per peer
  const safetyNumbersRef = useRef<Map<string, string>>(new Map())
  const [safetyNumbers, setSafetyNumbers] = useState<Map<string, string>>(new Map())

  const signalingRef = useRef(signaling)
  signalingRef.current = signaling

  const updatePeerStates = useCallback(() => {
    setPeerStates(new Map(peerStatesRef.current))
  }, [])

  const updateSafetyNumbers = useCallback(() => {
    setSafetyNumbers(new Map(safetyNumbersRef.current))
  }, [])

  // ── Worker Lifecycle ───────────────────────────────────────────────

  useEffect(() => {
    if (!enabled) return

    const worker = new Worker(new URL('../workers/e2ee-worker.ts', import.meta.url), { type: 'module' })
    workerRef.current = worker

    return () => {
      worker.terminate()
      workerRef.current = null
    }
  }, [enabled])

  // ── Key Generation on Mount ────────────────────────────────────────

  useEffect(() => {
    if (!enabled) return

    async function init() {
      ecdhKeyPairRef.current = await generateECDHKeyPair()
      senderKeyRawRef.current = await generateSenderKey()
      localKeyIdRef.current = 0
      setLocalKeyId(0)

      // Set our own sender key in the worker
      workerRef.current?.postMessage({
        type: 'setKey',
        participantId,
        keyId: 0,
        rawKey: senderKeyRawRef.current,
      })
    }

    init()
  }, [enabled, participantId])

  // ── Send public key to room ────────────────────────────────────────

  const broadcastPublicKey = useCallback(async () => {
    if (!enabled || !ecdhKeyPairRef.current) return
    const pubKeyBase64 = await exportPublicKey(ecdhKeyPairRef.current.publicKey)
    signalingRef.current.send({
      type: 'e2ee_public_key',
      from: participantId,
      room_id: roomId,
      public_key: pubKeyBase64,
    } as SignalingMessage)
  }, [enabled, participantId, roomId])

  // ── Exchange sender key with a specific peer ───────────────────────

  const sendSenderKeyTo = useCallback(async (peerId: string) => {
    const sharedSecret = sharedSecretsRef.current.get(peerId)
    const senderKeyRaw = senderKeyRawRef.current
    if (!sharedSecret || !senderKeyRaw) return

    const encrypted = await encryptSenderKey(sharedSecret, senderKeyRaw)
    signalingRef.current.send({
      type: 'e2ee_sender_key',
      from: participantId,
      to: peerId,
      room_id: roomId,
      encrypted_key: encrypted,
      key_id: localKeyIdRef.current,
    } as SignalingMessage)
  }, [participantId, roomId])

  // ── Rotate sender key (generate new, distribute to all peers) ──────

  const rotateSenderKey = useCallback(async () => {
    if (!enabled || !senderKeyRawRef.current) return
    const newKeyId = localKeyIdRef.current + 1
    senderKeyRawRef.current = await generateSenderKey()
    localKeyIdRef.current = newKeyId
    setLocalKeyId(newKeyId)

    // Update worker with new key
    workerRef.current?.postMessage({
      type: 'setKey',
      participantId,
      keyId: newKeyId,
      rawKey: senderKeyRawRef.current,
    })

    // Purge old keys after a delay (allow in-transit frames)
    setTimeout(() => {
      workerRef.current?.postMessage({
        type: 'purgeOldKeys',
        participantId,
        keepKeyId: newKeyId,
      })
    }, 5000)

    // Send new key to all peers
    for (const peerId of sharedSecretsRef.current.keys()) {
      await sendSenderKeyTo(peerId)
    }
  }, [enabled, participantId, sendSenderKeyTo])

  // ── DH Ratchet (every 2 minutes) ─────────────────────────────────
  //
  // Provides Post-Compromise Security (PCS) by injecting fresh entropy.
  // Unlike the old HKDF ratchet (which derived from the previous key),
  // this generates a new ECDH keypair and fresh random sender key.
  // An attacker who compromised the old key cannot derive future keys
  // because the new shared secrets include fresh DH entropy.

  useEffect(() => {
    if (!enabled) return
    const interval = setInterval(async () => {
      if (!senderKeyRawRef.current) return

      // 1. Generate new ECDH keypair (fresh entropy for PCS)
      ecdhKeyPairRef.current = await generateECDHKeyPair()

      // 2. Generate fresh random sender key (not derived from old key)
      const newKeyId = localKeyIdRef.current + 1
      senderKeyRawRef.current = await generateSenderKey()
      localKeyIdRef.current = newKeyId
      setLocalKeyId(newKeyId)

      // 3. Set new key in worker
      workerRef.current?.postMessage({
        type: 'setKey',
        participantId,
        keyId: newKeyId,
        rawKey: senderKeyRawRef.current,
      })

      // 4. Purge old keys after a delay
      setTimeout(() => {
        workerRef.current?.postMessage({
          type: 'purgeOldKeys',
          participantId,
          keepKeyId: newKeyId,
        })
      }, 5000)

      // 5. Clear old shared secrets (they used the old ECDH keypair)
      sharedSecretsRef.current.clear()

      // 6. Broadcast new public key — peers will:
      //    a) Derive new shared secret (their private + our new public)
      //    b) Respond with their public key
      //    c) We re-derive shared secret and send new sender key
      await broadcastPublicKey()

      console.log(`[E2EE] DH ratchet completed, new keyId=${newKeyId}`)
    }, 2 * 60 * 1000)

    return () => clearInterval(interval)
  }, [enabled, participantId, roomId, broadcastPublicKey])

  // ── Handle E2EE signaling messages ─────────────────────────────────

  const handleMessage = useCallback(async (msg: SignalingMessage) => {
    if (!enabled) return

    if (msg.type === 'e2ee_public_key') {
      const senderId = msg.from
      if (senderId === participantId) return

      // Import their public key
      const peerPubKey = await importPublicKey(msg.public_key)
      peerPublicKeysRef.current.set(senderId, peerPubKey)

      // Derive shared secret
      if (ecdhKeyPairRef.current) {
        const sharedSecret = await deriveSharedSecret(ecdhKeyPairRef.current.privateKey, peerPubKey)
        sharedSecretsRef.current.set(senderId, sharedSecret)

        // Compute safety number for this peer
        const safetyNumber = await computeSafetyNumber(ecdhKeyPairRef.current.publicKey, peerPubKey)
        safetyNumbersRef.current.set(senderId, safetyNumber)
        updateSafetyNumbers()

        // Reply with our own public key (they may not have it yet)
        await broadcastPublicKey()

        // Send our sender key
        await sendSenderKeyTo(senderId)
      }
    }

    if (msg.type === 'e2ee_sender_key') {
      if (msg.to !== participantId) return
      const senderId = msg.from

      const sharedSecret = sharedSecretsRef.current.get(senderId)
      if (!sharedSecret) {
        console.warn(`[E2EE] received sender key from ${senderId.slice(0, 8)} but no shared secret`)
        return
      }

      try {
        const senderKeyRaw = await decryptSenderKey(sharedSecret, msg.encrypted_key)

        // Store peer sender key for chat decryption
        peerSenderKeysRef.current.set(senderId, { raw: senderKeyRaw, keyId: msg.key_id })

        // Set in worker
        workerRef.current?.postMessage({
          type: 'setKey',
          participantId: senderId,
          keyId: msg.key_id,
          rawKey: senderKeyRaw,
        })

        // Update peer state
        peerStatesRef.current.set(senderId, { ready: true, keyId: msg.key_id })
        updatePeerStates()
        console.log(`[E2EE] sender key received from ${senderId.slice(0, 8)}, keyId=${msg.key_id}`)
      } catch (e) {
        console.error(`[E2EE] failed to decrypt sender key from ${senderId.slice(0, 8)}:`, e)
      }
    }

    if (msg.type === 'e2ee_key_rotation') {
      const senderId = msg.from
      if (senderId === participantId) return

      // The sender ratcheted — we need their new sender key via e2ee_sender_key.
      // The sender will re-send it after rotation. Mark as pending.
      const currentState = peerStatesRef.current.get(senderId)
      if (currentState) {
        peerStatesRef.current.set(senderId, { ...currentState, keyId: msg.key_id })
        updatePeerStates()
      }
    }
  }, [enabled, participantId, broadcastPublicKey, sendSenderKeyTo, updatePeerStates, updateSafetyNumbers])

  // ── Peer lifecycle callbacks ───────────────────────────────────────

  const onPeerJoined = useCallback(async (peerId: string) => {
    if (!enabled) return
    peerStatesRef.current.set(peerId, { ready: false, keyId: -1 })
    updatePeerStates()
    // Broadcast public key so the new peer can derive shared secret
    await broadcastPublicKey()
    // Rotate key so the new peer can't decrypt historical frames
    await rotateSenderKey()
  }, [enabled, broadcastPublicKey, rotateSenderKey, updatePeerStates])

  const onPeerLeft = useCallback(async (peerId: string) => {
    if (!enabled) return
    peerStatesRef.current.delete(peerId)
    peerPublicKeysRef.current.delete(peerId)
    sharedSecretsRef.current.delete(peerId)
    peerSenderKeysRef.current.delete(peerId)
    safetyNumbersRef.current.delete(peerId)
    updatePeerStates()
    updateSafetyNumbers()

    // Remove departed peer's keys from worker
    workerRef.current?.postMessage({ type: 'removeKeys', participantId: peerId })

    // Rotate key so the departed peer can't decrypt future frames
    await rotateSenderKey()
  }, [enabled, rotateSenderKey, updatePeerStates, updateSafetyNumbers])

  // ── Chat encryption / decryption ──────────────────────────────────

  const encryptChat = useCallback(async (content: string): Promise<{ ciphertext: string; keyId: number } | null> => {
    if (!enabled || !senderKeyRawRef.current) return null
    try {
      const key = await crypto.subtle.importKey('raw', senderKeyRawRef.current, { name: 'AES-GCM' }, false, ['encrypt'])
      const iv = crypto.getRandomValues(new Uint8Array(12))
      const encoded = new TextEncoder().encode(content)
      const ciphertext = await crypto.subtle.encrypt({ name: 'AES-GCM', iv, tagLength: 128 }, key, encoded)
      const packed = new Uint8Array(12 + ciphertext.byteLength)
      packed.set(iv, 0)
      packed.set(new Uint8Array(ciphertext), 12)
      return { ciphertext: btoa(String.fromCharCode(...packed)), keyId: localKeyIdRef.current }
    } catch (e) {
      console.error('[E2EE] encryptChat failed:', e)
      return null
    }
  }, [enabled])

  const decryptChat = useCallback(async (from: string, ciphertext: string, keyId: number): Promise<string | null> => {
    if (!enabled) return null
    // Use own sender key if the message is from ourselves (echo)
    let rawKey: ArrayBuffer | undefined
    if (from === participantId) {
      rawKey = senderKeyRawRef.current ?? undefined
    } else {
      const peerKey = peerSenderKeysRef.current.get(from)
      if (peerKey) rawKey = peerKey.raw
    }
    if (!rawKey) return null
    try {
      const key = await crypto.subtle.importKey('raw', rawKey, { name: 'AES-GCM' }, false, ['decrypt'])
      const binary = atob(ciphertext)
      const packed = new Uint8Array(binary.length)
      for (let i = 0; i < binary.length; i++) packed[i] = binary.charCodeAt(i)
      const iv = packed.slice(0, 12)
      const data = packed.slice(12)
      const decrypted = await crypto.subtle.decrypt({ name: 'AES-GCM', iv, tagLength: 128 }, key, data)
      return new TextDecoder().decode(decrypted)
    } catch (e) {
      console.warn(`[E2EE] decryptChat failed for ${from.slice(0, 8)}, keyId=${keyId}:`, e)
      return null
    }
  }, [enabled, participantId])

  return {
    worker: workerRef.current,
    peerStates,
    enabled,
    handleMessage,
    onPeerLeft,
    onPeerJoined,
    localKeyId,
    encryptChat,
    decryptChat,
    safetyNumbers,
  }
}
