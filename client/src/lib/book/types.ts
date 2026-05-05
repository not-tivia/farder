export interface BookItem {
  id: string;
  name: string;
  ext: "png" | "jpg" | "jpeg" | "gif" | "webp";
  width?: number;
  height?: number;
  animated: boolean;
  added_at: number;
  source: "upload" | "chat" | "favorites-migration";
  server_files: Record<string, number>;
}

export type BookFilter = "all" | "static" | "animated";
export type BookSort = "recent" | "alpha";
