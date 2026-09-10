import { render } from '@solidjs/web';
import { createSignal } from 'solid-js';
import { ArtifactPreview } from '../src/artifacts/ArtifactPreview';
import type { Artifact } from '../src/artifacts/types';
const solid = (content: string): Artifact => ({id:1,title:'Counter',kind:'solid',mime:'text/jsx',thread_ids:[3],content,created_at:'now',updated_at:'now'});
const [artifact, setArtifact] = createSignal(solid(`import {createSignal} from 'solid-js'; export default function App(){const [count,setCount]=createSignal(0);return <button onClick={()=>setCount(count()+1)}>Count {count()}</button>}`));
Object.assign(window,{
  replaceArtifact: (content: string) => setArtifact(solid(content)),
  showArtifact: (next: Artifact) => setArtifact(next),
});
render(()=><div style="height:600px"><ArtifactPreview artifact={artifact()}/></div>,document.getElementById('root')!);
