<script lang="ts">
  import tokenLedgerMark from '../assets/tokenledger-omp-mark.png';

  interface Props {
    size?: number;
    decorative?: boolean;
  }

  let { size = 18, decorative = true }: Props = $props();
</script>

<img
  src={tokenLedgerMark}
  width={size}
  height={size}
  alt={decorative ? '' : 'TokenLedger OMP'}
  aria-hidden={decorative}
  draggable="false"
/>

<style>
  img {
    display: block;
    object-fit: contain;
  }
</style>
