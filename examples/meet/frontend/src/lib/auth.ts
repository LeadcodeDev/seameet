/**
 * Returns the bearer credential the frontend will send to seameet (in the
 * `Authorization` header on `POST /rooms/:room_id/participants`, and as
 * the `token` field of the WebSocket `Join` payload).
 *
 * INTEGRATORS: replace this body with a call to your IAM. Typical patterns:
 *
 *   - silent refresh from an OIDC SDK (Auth0, Keycloak, Clerk, …)
 *   - read a JWT from an HTTP-only cookie set by your backend
 *   - exchange a longer-lived refresh token for a short-lived access token
 *
 * The seameet server's `on_authenticate` hook receives this exact string
 * and is the place where signature/issuer/audience/exp are validated.
 *
 * The example server only checks that the token is non-empty, so this
 * stub is enough to exercise the integration contract end-to-end.
 */
export async function getAuthToken(): Promise<string> {
  return `seameet-dev-${crypto.randomUUID()}`
}
