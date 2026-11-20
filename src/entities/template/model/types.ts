/** Where each profile made from a template gets a fingerprint of its own
 *  (templates.rs `Source`): that library fingerprint while this computer has
 *  it, else any of `os`. Never a fingerprint itself. */
export type TemplateSource = { library: string | null; os: string };

/** A profile template (#145), as the backend keeps it (templates.rs). */
export type ProfileTemplate = {
  id: string;
  name: string;
  source: TemplateSource;
  /** What a new profile takes, as a stored profile holds it (`templateSettingsOf`'s keys). */
  settings: Record<string, any>;
  folder: string;
  /** Tag ids of the space the template is in. */
  tags: string[];
  /** "New profile" starts from it; one per space. */
  is_default: boolean;
  /** Unix seconds of the last save. */
  updated_at: number;
};
