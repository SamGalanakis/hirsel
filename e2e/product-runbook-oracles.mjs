export function renderedInlineCodeText(markdown) {
  return markdown.replace(/`([^`\r\n]+)`/g, "$1");
}
