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
}
