import { invoke } from "@tauri-apps/api/core";

export async function generateKeypair(): Promise<string> {
  return invoke<string>("generate_keypair");
}
export async function getPublicKey(): Promise<string> {
  return invoke<string>("get_public_key");
}
export async function exportKey(passphrase: string): Promise<number[]> {
  return invoke<number[]>("export_key", { passphrase });
}
export async function importKey(data: number[], passphrase: string): Promise<string> {
  return invoke<string>("import_key", { data, passphrase });
}
