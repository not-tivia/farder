import { invoke } from "@tauri-apps/api/core";
import type { BookItem } from "./types";

export async function bookListItems(): Promise<BookItem[]> {
  return invoke<BookItem[]>("book_list_items");
}

export async function bookUploadItem(sourcePath: string, name?: string): Promise<BookItem> {
  return invoke<BookItem>("book_upload_item", { sourcePath, name: name ?? null });
}

export async function bookDeleteItem(id: string): Promise<void> {
  return invoke<void>("book_delete_item", { id });
}

export async function bookRenameItem(id: string, newName: string): Promise<BookItem> {
  return invoke<BookItem>("book_rename_item", { id, newName });
}

export async function bookGetFileForServer(serverId: string, itemId: string): Promise<number> {
  return invoke<number>("book_get_file_for_server", { serverId, itemId });
}

export async function bookMigrateLegacyFavorites(): Promise<number> {
  return invoke<number>("book_migrate_legacy_favorites");
}

export async function bookSaveFromUrl(serverId: string, fileId: number, name?: string): Promise<BookItem> {
  return invoke<BookItem>("book_save_from_url", { serverId, fileId, name: name ?? null });
}

export async function bookItemAbsolutePath(id: string): Promise<string> {
  return invoke<string>("book_item_absolute_path", { id });
}
