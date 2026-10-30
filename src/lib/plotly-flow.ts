// Plotly build for Writer Flow: the 2D traces plus heatmap, which only this
// page draws (strikes by time). Kept apart from plotly-2d.ts so the other 2D
// tools do not load the heatmap code, the same way plotly-3d.ts keeps WebGL
// to VolSurface.

import bar from 'plotly.js/lib/bar'
import Plotly from 'plotly.js/lib/core'
import heatmap from 'plotly.js/lib/heatmap'
import scatter from 'plotly.js/lib/scatter'

Plotly.register([scatter, bar, heatmap])

export default Plotly
