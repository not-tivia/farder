/* tslint:disable */
/* eslint-disable */

/**
 * A throwaway guest identity.
 */
export class GuestKey {
    free(): void;
    [Symbol.dispose](): void;
    /**
     * Answer the server's Challenge frame: returns the Authenticate frame to send. The guest's own
     * key signs it. `invite` is the guest link code (empty for a returning guest).
     */
    answer_challenge(challenge_frame: Uint8Array, invite: string): Uint8Array;
    /**
     * Join step 1 on a server with a signed history: the request frame that authorizes this guest's
     * device (a key made from the guest's own, so only one secret is kept). `now` is seconds since 1970.
     */
    begin_join(id: number, log_server_id: string, now: number): Uint8Array;
    /**
     * Join step 2: the request frame saying "I joined with this invite" (`invite_event` is what the
     * server answered to ResolveInvite). Call after step 1 was accepted.
     */
    finish_join(id: number, log_server_id: string, invite_event: string, now: number): Uint8Array;
    /**
     * Rebuild a key the page saved earlier (so a refresh does not log the guest out).
     */
    static from_secret(secret: Uint8Array): GuestKey;
    /**
     * A new random key (from the browser's secure random source).
     */
    constructor();
    /**
     * The request frame that sets this guest's display name (signed by the guest's key).
     */
    profile_request(id: number, display_name: string): Uint8Array;
    /**
     * The public key as the app shows it (`vk_` and 64 hex characters).
     */
    public_key(): string;
    /**
     * The secret, for the page to keep in the browser's own storage. Never send it anywhere.
     */
    secret(): Uint8Array;
}

/**
 * Turn one frame from the server into readable JSON text.
 */
export function decode_server_frame(bytes: Uint8Array): string;

/**
 * Turn a request written as JSON (for example `{"SendMessage":{"channel_id":1,"content":"hi",
 * "reply_to":null,"attachment_ids":[]}}` or `"GetServerInfo"`) into the bytes for one request frame.
 */
export function encode_request(id: number, request_json: string): Uint8Array;

export type InitInput = RequestInfo | URL | Response | BufferSource | WebAssembly.Module;

export interface InitOutput {
    readonly memory: WebAssembly.Memory;
    readonly __wbg_guestkey_free: (a: number, b: number) => void;
    readonly decode_server_frame: (a: number, b: number) => [number, number, number, number];
    readonly encode_request: (a: number, b: number, c: number) => [number, number, number, number];
    readonly guestkey_answer_challenge: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly guestkey_begin_join: (a: number, b: number, c: number, d: number, e: number) => [number, number, number, number];
    readonly guestkey_finish_join: (a: number, b: number, c: number, d: number, e: number, f: number, g: number) => [number, number, number, number];
    readonly guestkey_from_secret: (a: number, b: number) => [number, number, number];
    readonly guestkey_new: () => number;
    readonly guestkey_profile_request: (a: number, b: number, c: number, d: number) => [number, number, number, number];
    readonly guestkey_public_key: (a: number) => [number, number];
    readonly guestkey_secret: (a: number) => [number, number];
    readonly __wbindgen_exn_store: (a: number) => void;
    readonly __externref_table_alloc: () => number;
    readonly __wbindgen_externrefs: WebAssembly.Table;
    readonly __wbindgen_malloc: (a: number, b: number) => number;
    readonly __externref_table_dealloc: (a: number) => void;
    readonly __wbindgen_free: (a: number, b: number, c: number) => void;
    readonly __wbindgen_realloc: (a: number, b: number, c: number, d: number) => number;
    readonly __wbindgen_start: () => void;
}

export type SyncInitInput = BufferSource | WebAssembly.Module;

/**
 * Instantiates the given `module`, which can either be bytes or
 * a precompiled `WebAssembly.Module`.
 *
 * @param {{ module: SyncInitInput }} module - Passing `SyncInitInput` directly is deprecated.
 *
 * @returns {InitOutput}
 */
export function initSync(module: { module: SyncInitInput } | SyncInitInput): InitOutput;

/**
 * If `module_or_path` is {RequestInfo} or {URL}, makes a request and
 * for everything else, calls `WebAssembly.instantiate` directly.
 *
 * @param {{ module_or_path: InitInput | Promise<InitInput> }} module_or_path - Passing `InitInput` directly is deprecated.
 *
 * @returns {Promise<InitOutput>}
 */
export default function __wbg_init (module_or_path?: { module_or_path: InitInput | Promise<InitInput> } | InitInput | Promise<InitInput>): Promise<InitOutput>;
