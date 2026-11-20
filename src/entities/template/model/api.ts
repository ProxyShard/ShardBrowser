import { invoke } from "../../../shared/api/invoke";
import type { ProfileTemplate } from "./types";

/** The space's templates, by name. */
export const templateList = () => invoke<ProfileTemplate[]>("profile_template_list");
/** Adds the template or changes the one with its id; the backend cuts its settings to what a template carries. */
export const templateSave = (template: ProfileTemplate) => invoke<ProfileTemplate>("profile_template_save", { template });
export const templateDelete = (id: string) => invoke<null>("profile_template_delete", { id });
