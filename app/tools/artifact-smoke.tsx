import { render } from '@solidjs/web';
import { createSignal } from 'solid-js';
import { ArtifactPreview } from '../src/artifacts/ArtifactPreview';
const [content, setContent] = createSignal(`import {createSignal} from 'solid-js'; export default function App(){const [count,setCount]=createSignal(0);return <button onClick={()=>setCount(count()+1)}>Count {count()}</button>}`);
Object.assign(window,{replaceArtifact: setContent});
render(()=><div style="height:600px"><ArtifactPreview artifact={{id:1,title:'Counter',kind:'solid',mime:'text/jsx',thread_ids:[3],content:content(),created_at:'now',updated_at:'now'}}/></div>,document.getElementById('root')!);
