/** 生成带省略号的 1-based 分页页码。 */
export function getPageNumbers(currentPage: number, totalPages: number): Array<number | '...'> {
  const maxVisiblePages = 5;
  const pages: Array<number | '...'> = [];

  if (totalPages <= 1) {
    return [1];
  }

  if (totalPages <= maxVisiblePages) {
    for (let page = 1; page <= totalPages; page += 1) {
      pages.push(page);
    }
    return pages;
  }

  pages.push(1);
  if (currentPage <= 3) {
    for (let page = 2; page <= 4; page += 1) {
      pages.push(page);
    }
    pages.push('...', totalPages);
    return pages;
  }
  if (currentPage >= totalPages - 2) {
    pages.push('...');
    for (let page = totalPages - 3; page <= totalPages; page += 1) {
      pages.push(page);
    }
    return pages;
  }
  pages.push('...');
  for (let page = currentPage - 1; page <= currentPage + 1; page += 1) {
    pages.push(page);
  }
  pages.push('...', totalPages);
  return pages;
}
