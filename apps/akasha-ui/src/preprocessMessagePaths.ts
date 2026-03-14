const IMAGE_EXT = /\.(png|jpe?g|gif|webp)$/i;

/** Replaces local file/folder paths in text with markdown links [path](path:ENCODED) for clickable handling. Image paths use pathfolder: so click opens the folder. */
export function preprocessMessagePaths(text: string): string {
  if (!text || typeof text !== "string") return text;
  // (?<!<)\/ and (?!>) prevent matching "/" inside HTML tags (e.g. "</div>" or " />")
  // so preprocessDataUrlImages output is not corrupted when this runs after it.
  const pathRegex = /(^|[\s([])((?:[A-Za-z]:\\[^\s)\]]+)|(?:(?<!<)\/(?!>)(?:[^\s/]+(?:\/[^\s/]*)*)))(?=[\s).,!?\]:]|$)/g;
  return text.replace(pathRegex, (_, before, path) => {
    const prefix = IMAGE_EXT.test(path) ? "pathfolder:" : "path:";
    return before + "[`" + path + "`](" + prefix + encodeURIComponent(path) + ")";
  });
}
