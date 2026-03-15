const IMAGE_EXT = /\.(png|jpe?g|gif|webp)$/i;

/** Replaces local file/folder paths in text with markdown links [path](path:ENCODED) for clickable handling. Image paths use pathfolder: so click opens the folder. */
export function preprocessMessagePaths(text: string): string {
  if (!text || typeof text !== "string") return text;
  // Adding "<" to before-group and guarding in the callback avoids matching "/" inside
  // HTML tags (e.g. "</div>" or " />") without using negative lookbehind (unsupported
  // in Safari 13 and other older engines). "(?!>)" already excludes self-closing "/>".
  const pathRegex = /(^|[\s([<])((?:[A-Za-z]:\\[^\s)\]]+)|(?:\/(?!>)(?:[^\s/]+(?:\/[^\s/]*)*)))(?=[\s).,!?\]:]|$)/g;
  return text.replace(pathRegex, (_, before, path) => {
    if (before === "<") return before + path;
    const prefix = IMAGE_EXT.test(path) ? "pathfolder:" : "path:";
    return before + "[`" + path + "`](" + prefix + encodeURIComponent(path) + ")";
  });
}
