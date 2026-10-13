const stack: symbol[] = []

export function pushModal(id: symbol) {
  if (!stack.includes(id)) stack.push(id)
}

export function removeModal(id: symbol) {
  const i = stack.indexOf(id)
  if (i !== -1) stack.splice(i, 1)
}

export function isTopModal(id: symbol) {
  return stack[stack.length - 1] === id
}
