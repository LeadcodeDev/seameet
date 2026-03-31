/** Returns true if the browser supports Encoded Transforms (RTCRtpScriptTransform). */
export function isE2EESupported(): boolean {
  return typeof RTCRtpScriptTransform !== 'undefined'
}
