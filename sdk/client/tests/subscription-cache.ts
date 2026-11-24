import type {bind} from './generated/entries.js';

type Api = ReturnType<typeof bind>;
type Subscribe = Api['subscribeDecoded']['packed_consumer']['packed_rows'];
type Items = ReturnType<Subscribe>;
type Change = Parameters<Parameters<Subscribe>[1]>[1];

export function watchItem(api: Api, id: bigint,
  display: (sub: Items, change: Change, text: string) => void, onEnd: Parameters<Subscribe>[2]) {
  return api.subscribeDecoded.packed_consumer.packed_rows(undefined, (sub, change) => {
    if (change.invalidated) return display(sub, change, 'Reconnecting');
    const item = sub.get('items', id);
    const items = sub.map('items');
    if (items.tag === 'Ready' && item.tag === 'Ready' && item.value?.value.tag === 'Some') {
      const {title, done} = item.value.value.value;
      display(sub, change, `${title}: ${done ? 'done' : 'open'} (total: ${items.value.size})`);
    }
  }, onEnd);
}

export function reopenWhileCalling(api: Api, sub: Items, id: bigint) {
  return api.call.packed_consumer.packed_reply(undefined, {optimistic(draft) {
    const item = draft.get(sub, 'items', id);
    if (item?.value.tag === 'Some') {
      draft.put(sub, 'items', id, {...item, value: {tag: 'Some', value: {...item.value.value, done: false}}});
    }
  }});
}
