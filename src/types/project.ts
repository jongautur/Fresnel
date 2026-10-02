export interface Project {
  id: number;
  name: string;
  customerName: string | null;
  createdAt: string;
  updatedAt: string;
}

export interface NewProject {
  name: string;
  customerName: string | null;
}

export interface AppInfo {
  version: string;
  databasePath: string;
  databaseError: string | null;
  schemaVersion: number | null;
  /** Set when something happened to the database the user should know about (e.g. a damaged file was set aside). */
  databaseNotice: string | null;
}
