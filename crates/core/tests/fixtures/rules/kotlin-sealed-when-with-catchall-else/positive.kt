fun handle(state: UiState) {
    when (state) {
        is UiState.Loading -> showSpinner()
        is UiState.Error -> showError()
        else -> {}
    }
}
