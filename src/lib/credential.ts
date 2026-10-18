import type { CredentialPresence } from './contracts/index';

export type { CredentialDescriptor } from './contracts/index';

/** Saves a replacement secret or removes it when the value is null. */
export type SaveCredential = (value: string | null) => Promise<CredentialPresence>;
