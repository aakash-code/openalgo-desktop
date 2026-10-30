// React component bound to the Writer Flow Plotly build (2D traces plus
// heatmap). Same factory unwrap as Plot2D.tsx, for the same Vite interop reason.
import createPlotlyComponent from 'react-plotly.js/factory'
import Plotly from './plotly-flow'

// eslint-disable-next-line @typescript-eslint/no-explicit-any
const factory = createPlotlyComponent as any
// eslint-disable-next-line @typescript-eslint/no-explicit-any
const Plot = (factory.default ?? factory)(Plotly as any)

export default Plot
